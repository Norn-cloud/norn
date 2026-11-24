import { queryOptions, replaceEqualDeep, useMutation, useQuery, useQueryClient, type Query, type QueryClient, type QueryKey, type UseMutationResult, type UseQueryResult } from "@tanstack/react-query"
import { useEffect, useState } from "react"
import { createNornClient, fillRoute, NornRefusalError, NornUnavailableError, scopedQuery, type CallOptions, type FetchLike, type GetRoute, type NornClient, type NornDocument, type Operation, type PostRoute, type Success } from "../../../../packages/norn-sdk/src/client.ts"
import { readExact } from "../../../../packages/norn-sdk/src/exact.ts"
import type { LiveRefusal, LiveSnapshot } from "../../../../packages/norn-demo-ui/src/live/client.ts"
import { cellPersistence, forgetOnScopeChange, type CellPersistence, type StoredDocument } from "./cell-cache.ts"
import { cellCacheStorage } from "./cell-cache-store.ts"
import { CELL_FETCH_TIMEOUT_MS, CellUnavailable, fetchWithTimeout, NO_DATA_CELL, parseUnavailableDetail, recordCellReachable, StudioScopeError, type StudioScope } from "./definitions.ts"
import { live } from "./live-reads.ts"
import { CellRefusal, NotServed } from "./live-transport.ts"

/**
 * One read primitive for the cell's documents (#1374): `useCellQuery(route, params, { scope })` is
 * a TanStack query over the typed client (`@norn/sdk`) joined to the page's live socket. The cell
 * pushes a changed answer and the pushed document is written into the cache (`setQueryData`):
 * nothing is read again and nothing polls. Studio renders what the cell sends and decides nothing.
 *
 * - The key is `[route, orgId, environmentId, params]`: reads of two scopes never share an entry.
 * - A refusal (read or pushed) is the existing refusal state: a {@link CellRefusal},
 *   {@link CellUnavailable} or {@link NotServed} error, as `cellRead` raises them.
 * - Unmounting unsubscribes; the live client keeps its own limits (frame size, one wire
 *   subscription per path, the cell's `live.too_many_subscriptions`).
 * - Persistence across reloads is opt-in per query and only for documents the server marks
 *   storable (`cell-cache.ts`).
 */

/** A cell document as the cache holds it: Studio's `Read<T>` shape, plus where it came from. */
export interface CellDocument<T> {
  readonly status: number
  readonly document: T
  readonly numbers: ReadonlyMap<string, string>
  readonly origin: "live"
  /** The server's text when this came from a read; null for a pushed document. */
  readonly text: string | null
  readonly cacheControl: string | null
  /** The live digest of a pushed document. */
  readonly digest: string | null
}

/** The parameters of one read: its path parameters and query, without the scope. */
export type CellParams<P extends GetRoute> = Pick<CallOptions<Operation<P, "get">>, "path" | "query">
export type CellData<P extends GetRoute> = CellDocument<Success<Operation<P, "get">>>
export type CellQueryKey = readonly [route: string, orgId: string, environmentId: string, params: unknown]

const NO_SCOPE: StudioScope = { orgId: "", environmentId: "" }

export const cellQueryKey = (route: string, scope: StudioScope, params: unknown): CellQueryKey =>
  [route, scope.orgId, scope.environmentId, params ?? {}]

const browserFetch: FetchLike = (input, init) => fetch(input, init)

/** The typed client through Studio's host proxy, as the signed-in account; reads keep Studio's read deadline. */
export function studioCellClient(scope: StudioScope, fetcher: FetchLike = browserFetch): NornClient {
  return createNornClient({
    baseUrl: "/api/host",
    scope,
    credentials: "same-origin",
    fetch: async (input, init) => {
      const method = init?.method?.toUpperCase() ?? "GET"
      if (method === "GET") return fetchWithTimeout(fetcher, input, init, CELL_FETCH_TIMEOUT_MS, () => recordCellReachable(scope))
      const response = await fetcher(input, init)
      if (response.ok) recordCellReachable(scope)
      return response
    },
  })
}

/** The scoped path a live subscription names, as the GET asks it. */
export const cellPath = (route: string, scope: StudioScope, params: { readonly path?: unknown; readonly query?: unknown } = {}): string =>
  `${fillRoute(route, params.path as Readonly<Record<string, unknown>> | undefined)}?${scopedQuery(params.query as Readonly<Record<string, unknown>> | undefined, scope)}`

// The Studio Worker's answer when no cell host is configured (live-transport.ts `NO_HOST`).
const NO_HOST = "No Norn host is connected to this deployment."

/** The existing refusal state for an answer the cell (or Studio's Worker) gave with `status`. */
function answerError(status: number, body: unknown, what: string): Error {
  const record = body !== null && typeof body === "object" && !Array.isArray(body) ? body as Record<string, unknown> : null
  const code = typeof record?.["code"] === "string" ? record["code"] : undefined
  if (status === 404 && (code === undefined || code === "route.not_found")) return new NotServed(what)
  if (status === 503 && record?.["error"] === NO_HOST) return new NotServed(what)
  const detail = parseUnavailableDetail(record)
  const message = typeof record?.["message"] === "string" && record["message"] !== "" ? record["message"] : undefined
  if (status === 503) return new CellUnavailable(status, message, detail)
  if (code !== undefined) return new CellRefusal(status, code, message ?? code, detail)
  return new CellUnavailable(status, message, detail)
}

/** The typed client's error as the existing refusal state, the way `cellRead` raises it. */
export function studioError(error: unknown, what: string): unknown {
  if (error instanceof NornRefusalError) return answerError(error.status, error.refusal, what)
  if (error instanceof NornUnavailableError) {
    if (error.status === 0) return error.cause instanceof CellUnavailable ? error.cause : new CellUnavailable(0, undefined, { retryable: true })
    let body: unknown = null
    try { body = error.body === null ? null : JSON.parse(error.body) } catch { body = null }
    return answerError(error.status, body, what)
  }
  return error
}

type LooseCall = (route: string, options?: { readonly path?: unknown; readonly query?: unknown; readonly body?: unknown; readonly signal?: AbortSignal }) => Promise<NornDocument<unknown>>

const asDocument = <T,>(read: NornDocument<unknown>): CellDocument<T> => ({
  status: read.status, document: read.document as T, numbers: read.numbers, origin: "live", text: read.text, cacheControl: read.cacheControl, digest: null,
})

/** Restore a kept document as a cell query's data. */
export const restoreDocument = (stored: StoredDocument): CellDocument<unknown> => ({
  status: stored.status, ...readExact<unknown>(stored.text), origin: "live", text: stored.text, cacheControl: null, digest: null,
})

/** Pushes per query hash: a read that started before a push never overwrites the pushed document. */
const pushes = new WeakMap<QueryClient, Map<string, number>>()
const pushCount = (client: QueryClient, hash: string): number => pushes.get(client)?.get(hash) ?? 0
function countPush(client: QueryClient, hash: string): void {
  let counts = pushes.get(client)
  if (counts === undefined) pushes.set(client, counts = new Map())
  counts.set(hash, (counts.get(hash) ?? 0) + 1)
}

let persistence: CellPersistence | null = null
const studioPersistence = (): CellPersistence => persistence ??= cellPersistence(cellCacheStorage(), restoreDocument)

export interface CellQueryConfig {
  readonly scope: StudioScope | null
  /** Keep the document across reloads when the server marks it storable. */
  readonly persist?: boolean
  readonly enabled?: boolean
  /** What the page calls this read in a "not served" state. */
  readonly what?: string
  readonly fetch?: FetchLike
  /** The persistence to use instead of Studio's IndexedDB one (tests). */
  readonly persistence?: CellPersistence
}

/** The query options of one cell read (what `useCellQuery` runs). */
export function cellQueryOptions<P extends GetRoute>(client: QueryClient, route: P, params: CellParams<P>, config: CellQueryConfig) {
  const scope = config.scope ?? NO_SCOPE
  const what = config.what ?? route
  const kept = config.persist === true ? config.persistence ?? studioPersistence() : undefined
  return queryOptions<CellData<P>, Error, CellData<P>, CellQueryKey>({
    queryKey: cellQueryKey(route, scope, params),
    enabled: config.scope !== null && config.enabled !== false,
    retry: false,
    ...(kept === undefined ? {} : { persister: kept.persister as never }),
    queryFn: async ({ queryKey, signal }) => {
      if (config.scope === null) throw new StudioScopeError(NO_DATA_CELL)
      const hash = JSON.stringify(queryKey)
      const before = pushCount(client, hash)
      let read: NornDocument<unknown>
      try {
        read = await (studioCellClient(config.scope, config.fetch).get as unknown as LooseCall)(route, { ...params, signal })
      } catch (error) {
        throw studioError(error, what)
      }
      // A push landed while this read was in flight: the pushed document is newer.
      const current = client.getQueryData<CellData<P>>(queryKey)
      if (pushCount(client, hash) !== before && current !== undefined) return current
      return asDocument(read)
    },
  })
}

/** The slice of the live client a read listens through (`LiveClient` is one). */
export interface LiveSource {
  subscribe(path: string, listener: (snapshot: LiveSnapshot) => void, options?: { readonly onRefused?: (refusal: LiveRefusal) => void }): () => void
}

const sameNumbers = (a: ReadonlyMap<string, string>, b: ReadonlyMap<string, string>) =>
  a.size === b.size && [...a].every(([pointer, text]) => b.get(pointer) === text)

/**
 * Join the read under `queryKey` to the cell's pushes for `path`: a changed answer is written into
 * the cache, a pushed refusal puts the read into its refusal state. Returns the unsubscribe.
 */
export function attachLive(options: {
  readonly client: QueryClient
  readonly live: LiveSource
  readonly scope: StudioScope
  readonly route: string
  readonly queryKey: CellQueryKey
  readonly path: string
  readonly what?: string
  readonly persistence?: CellPersistence
  readonly onRefused?: (refusal: LiveRefusal) => void
}): () => void {
  const { client, queryKey, route, scope } = options
  const hash = JSON.stringify(queryKey)
  return options.live.subscribe(options.path, (snapshot) => {
    const current = client.getQueryData<CellDocument<unknown>>(queryKey)
    if (snapshot.status >= 200 && snapshot.status < 300) {
      // The first answer after subscribing is the page's own read: nothing changed.
      if (current !== undefined && replaceEqualDeep(current.document, snapshot.body) === current.document && sameNumbers(current.numbers, snapshot.numbers)) return
      countPush(client, hash)
      const pushed: CellDocument<unknown> = { status: snapshot.status, document: snapshot.body, numbers: snapshot.numbers, origin: "live", text: null, cacheControl: null, digest: snapshot.digest }
      client.setQueryData(queryKey, pushed)
      void options.persistence?.write(scope, queryKey, route, pushed)
      return
    }
    countPush(client, hash)
    const query: Query | undefined = client.getQueryCache().find({ queryKey, exact: true })
    query?.setState({ status: "error", error: answerError(snapshot.status, snapshot.body, options.what ?? route), data: undefined, errorUpdatedAt: Date.now(), fetchStatus: "idle" })
    void options.persistence?.write(scope, queryKey, route, { status: snapshot.status, document: snapshot.body, text: null, cacheControl: null })
  }, options.onRefused ? { onRefused: options.onRefused } : undefined)
}

export interface CellQuery<T> {
  readonly read: UseQueryResult<CellDocument<T>, Error>
  /** The live protocol's refusal of this read's subscription (`live.too_many_subscriptions`, …): the read stands, without pushes. */
  readonly liveRefusal: LiveRefusal | null
}

/** Read `route` with `params` in `scope`, kept current by the cell's pushes. */
export function useCellQuery<P extends GetRoute>(route: P, params: CellParams<P>, config: CellQueryConfig): CellQuery<Success<Operation<P, "get">>> {
  const client = useQueryClient()
  const options = cellQueryOptions(client, route, params, config)
  const read = useQuery(options)
  const [liveRefusal, setLiveRefusal] = useState<LiveRefusal | null>(null)
  const { scope } = config
  const enabled = options.enabled === true
  const key = JSON.stringify(options.queryKey)
  const kept = config.persist === true ? config.persistence ?? studioPersistence() : undefined
  useEffect(() => {
    if (kept !== undefined) forgetOnScopeChange(client, kept.storage)
  }, [client, kept])
  useEffect(() => {
    setLiveRefusal(null)
    if (!enabled || scope === null || typeof WebSocket === "undefined") return
    return attachLive({
      client, live: live(scope), scope, route, queryKey: options.queryKey, path: cellPath(route, scope, params),
      ...(config.what === undefined ? {} : { what: config.what }),
      ...(kept === undefined ? {} : { persistence: kept }),
      onRefused: setLiveRefusal,
    })
    // The key's value, not its identity, decides the subscription.
  }, [client, key, enabled])
  return { read, liveRefusal }
}

/** Reads to invalidate after a write: queries whose route starts with `route` and whose path parameters include `path`. */
export interface CellRoutePrefix {
  readonly route: string
  readonly path?: Readonly<Record<string, string>>
}

/** The reads an action on `objectType` can change (as `invalidateActionReads`): its objects, the inbox, decisions. */
export const actionReads = (objectType: string): readonly CellRoutePrefix[] => [
  { route: "/v1/objects/{type}", path: { type: objectType } },
  { route: "/v1/inbox" },
  { route: "/v1/decisions" },
]

/** Invalidate the cell reads of `scope` under `prefixes`. */
export function invalidateCellRoutes(client: QueryClient, scope: StudioScope, prefixes: readonly CellRoutePrefix[]): Promise<void> {
  const matches = (queryKey: QueryKey) => {
    const [route, orgId, environmentId, params] = queryKey
    if (typeof route !== "string" || orgId !== scope.orgId || environmentId !== scope.environmentId) return false
    const path = (params as { readonly path?: Readonly<Record<string, unknown>> } | undefined)?.path ?? {}
    return prefixes.some((prefix) => route.startsWith(prefix.route) && Object.entries(prefix.path ?? {}).every(([name, value]) => path[name] === value))
  }
  return client.invalidateQueries({ predicate: (query) => matches(query.queryKey) })
}

/** The variables of one call: its path parameters, query and body. */
export type CellCall<P extends PostRoute> = Pick<CallOptions<Operation<P, "post">>, "path" | "query" | "body">
export type CellResult<P extends PostRoute> = CellDocument<Success<Operation<P, "post">>>

/**
 * Call `route` in `scope`. A refusal is the typed `NornRefusalError` (code, category, checker
 * diagnostics); on success the reads `invalidates` names are read again.
 */
export function useCellMutation<P extends PostRoute>(route: P, config: {
  readonly scope: StudioScope | null
  readonly invalidates?: (result: CellResult<P>, call: CellCall<P>) => readonly CellRoutePrefix[]
  readonly fetch?: FetchLike
}): UseMutationResult<CellResult<P>, Error, CellCall<P>> {
  const client = useQueryClient()
  return useMutation<CellResult<P>, Error, CellCall<P>>({
    mutationFn: async (call) => {
      if (config.scope === null) throw new StudioScopeError(NO_DATA_CELL)
      return asDocument(await (studioCellClient(config.scope, config.fetch).post as unknown as LooseCall)(route, call))
    },
    onSuccess: async (result, call) => {
      if (config.scope !== null && config.invalidates !== undefined) await invalidateCellRoutes(client, config.scope, config.invalidates(result, call))
    },
  })
}
