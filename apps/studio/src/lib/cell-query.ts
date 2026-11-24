import { hashKey, queryOptions, replaceEqualDeep, useMutation, useQuery, useQueryClient, type MutateOptions, type MutationOptions, type Query, type QueryClient, type QueryKey, type UseMutationResult, type UseQueryResult } from "@tanstack/react-query"
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react"
import { createNornClient, fillRoute, NornRefusalError, NornUnavailableError, scopedQuery, type CallOptions, type FetchLike, type GetRoute, type NornClient, type NornDocument, type Operation, type PostRoute, type Success } from "../../../../packages/norn-sdk/src/client.ts"
import type { LiveRefusal, LiveSnapshot } from "../../../../packages/norn-demo-ui/src/live/client.ts"
import { CELL_FETCH_TIMEOUT_MS, CellUnavailable, fetchWithTimeout, getDisplayedScope, NO_DATA_CELL, parseUnavailableDetail, recordCellReachable, sameStudioScope, scopeHeaders, StudioScopeError, WORKSPACE_CHANGED_MESSAGE, type StudioScope } from "./definitions.ts"
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
 * - Nothing is kept across reloads. Persistence lands with the server's cache-policy flag (#1374),
 *   keyed by reader and scope.
 */

/** A cell document as the cache holds it: Studio's `Read<T>` shape, plus where it came from. */
export interface CellDocument<T> {
  readonly status: number
  readonly document: T
  readonly numbers: ReadonlyMap<string, string>
  readonly origin: "live"
  /** The live digest of a pushed document; null for a read. */
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

/**
 * The typed client through Studio's host proxy, as the signed-in account; reads keep Studio's read
 * deadline. A write carries the expected-scope headers (#1643), so Studio's Worker refuses it
 * (`409 workspace.changed`) if the session's active workspace is no longer `scope`.
 */
export function studioCellClient(scope: StudioScope, fetcher: FetchLike = browserFetch): NornClient {
  return createNornClient({
    baseUrl: "/api/host",
    scope,
    credentials: "same-origin",
    fetch: async (input, init) => {
      const method = init?.method?.toUpperCase() ?? "GET"
      if (method === "GET") return fetchWithTimeout(fetcher, input, init, CELL_FETCH_TIMEOUT_MS, () => recordCellReachable(scope))
      const headers = new Headers(init?.headers)
      for (const [name, value] of Object.entries(scopeHeaders(scope))) headers.set(name, value)
      const response = await fetcher(input, { ...init, headers })
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

/**
 * The typed client's error as the existing refusal state. A typed refusal keeps its code and
 * detail. An answer the SDK did not accept as a refusal is never read for one: it is not served
 * (a 404 naming no route, or Studio's Worker with no host) or the cell is unavailable, with no
 * code and no detail.
 */
export function studioError(error: unknown, what: string): unknown {
  if (error instanceof NornRefusalError) return answerError(error.status, error.refusal, what)
  if (error instanceof NornUnavailableError) {
    if (error.status === 0) return error.cause instanceof CellUnavailable ? error.cause : new CellUnavailable(0, undefined, { retryable: true })
    let body: unknown = null
    try { body = error.body === null || error.body.trim() === "" ? null : JSON.parse(error.body) } catch { body = undefined }
    const record = body !== null && typeof body === "object" && !Array.isArray(body) ? body as Record<string, unknown> : null
    if (error.status === 404 && (body === null || (record !== null && (record["code"] === undefined || record["code"] === "route.not_found")))) return new NotServed(what)
    if (error.status === 503 && record?.["error"] === NO_HOST) return new NotServed(what)
    return new CellUnavailable(error.status, undefined, {})
  }
  return error
}

type LooseCall = (route: string, options?: { readonly path?: unknown; readonly query?: unknown; readonly body?: unknown; readonly signal?: AbortSignal }) => Promise<NornDocument<unknown>>

const asDocument = <T,>(read: NornDocument<unknown>): CellDocument<T> => ({
  status: read.status, document: read.document as T, numbers: read.numbers, origin: "live", digest: null,
})

/**
 * The live sequence of each read (per query hash): it advances on every frame the cell sends for
 * the read, a pushed snapshot or a pushed refusal, whether or not the document changed. A read
 * notes the sequence when it starts. If the sequence moved by the time the read answers, a push
 * landed while the read was in flight, so the read's answer may be older than the push and the
 * cache keeps what the push left. Arrival order on the one socket orders the frames: no value
 * comparison decides which answer is newer.
 */
const sequences = new WeakMap<QueryClient, Map<string, number>>()
const sequenceOf = (client: QueryClient, hash: string): number => sequences.get(client)?.get(hash) ?? 0
function advance(client: QueryClient, hash: string): void {
  let counts = sequences.get(client)
  if (counts === undefined) sequences.set(client, counts = new Map())
  counts.set(hash, (counts.get(hash) ?? 0) + 1)
}

export interface CellQueryConfig {
  readonly scope: StudioScope | null
  readonly enabled?: boolean
  /** What the page calls this read in a "not served" state. */
  readonly what?: string
  readonly fetch?: FetchLike
}

/** The query options of one cell read (what `useCellQuery` runs). */
export function cellQueryOptions<P extends GetRoute>(client: QueryClient, route: P, params: CellParams<P>, config: CellQueryConfig) {
  const scope = config.scope ?? NO_SCOPE
  const what = config.what ?? route
  return queryOptions<CellData<P>, Error, CellData<P>, CellQueryKey>({
    queryKey: cellQueryKey(route, scope, params),
    enabled: config.scope !== null && config.enabled !== false,
    retry: false,
    queryFn: async ({ queryKey, signal }) => {
      if (config.scope === null) throw new StudioScopeError(NO_DATA_CELL)
      const hash = hashKey(queryKey)
      const started = sequenceOf(client, hash)
      let read: NornDocument<unknown>
      // A frame the cell pushed while this read was in flight is at least as new as the read's answer.
      const pushedSince = (): CellData<P> | undefined => {
        if (sequenceOf(client, hash) === started) return undefined
        const pushed = client.getQueryState<CellData<P>, Error>(queryKey)
        if (pushed?.error != null) throw pushed.error
        return pushed?.data
      }
      try {
        read = await (studioCellClient(config.scope, config.fetch).get as unknown as LooseCall)(route, { ...params, signal })
      } catch (error) {
        const pushed = pushedSince()
        if (pushed !== undefined) return pushed
        throw studioError(error, what)
      }
      return pushedSince() ?? asDocument(read)
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
 *
 * - The live client hands a new listener what it last received for the path at once. That replay
 *   is not a new answer: every listener of the path writes this same entry, so the cache already
 *   holds it or something newer (a later read). It is ignored.
 * - `onLiveRefusal` reports the live protocol's refusal of the subscription, and `null` once the
 *   cell sends this subscription a frame again (after a reconnect).
 */
export function attachLive(options: {
  readonly client: QueryClient
  readonly live: LiveSource
  readonly route: string
  readonly queryKey: CellQueryKey
  readonly path: string
  readonly what?: string
  readonly onLiveRefusal?: (refusal: LiveRefusal | null) => void
}): () => void {
  const { client, queryKey, route, onLiveRefusal } = options
  const hash = hashKey(queryKey)
  let replaying = true
  const stop = options.live.subscribe(options.path, (snapshot) => {
    if (replaying) return
    advance(client, hash)
    onLiveRefusal?.(null)
    if (snapshot.status >= 200 && snapshot.status < 300) {
      const current = client.getQueryData<CellDocument<unknown>>(queryKey)
      // An unchanged answer (the first frame after subscribing is the page's own read) keeps the
      // cached document as it is; the sequence above still fences any read in flight.
      if (current !== undefined && replaceEqualDeep(current.document, snapshot.body) === current.document && sameNumbers(current.numbers, snapshot.numbers)) return
      const pushed: CellDocument<unknown> = { status: snapshot.status, document: snapshot.body, numbers: snapshot.numbers, origin: "live", digest: snapshot.digest }
      client.setQueryData(queryKey, pushed)
      return
    }
    const query: Query | undefined = client.getQueryCache().find({ queryKey, exact: true })
    if (query !== undefined) {
      const error = answerError(snapshot.status, snapshot.body, options.what ?? route)
      query.setState({ status: "error", error, data: undefined, errorUpdatedAt: Date.now(), errorUpdateCount: query.state.errorUpdateCount + 1, fetchStatus: "idle" })
    }
  }, onLiveRefusal ? { onRefused: onLiveRefusal } : undefined)
  replaying = false
  return stop
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
  useEffect(() => {
    setLiveRefusal(null)
    if (!enabled || scope === null || typeof WebSocket === "undefined") return
    return attachLive({
      client, live: live(scope), route, queryKey: options.queryKey, path: cellPath(route, scope, params),
      ...(config.what === undefined ? {} : { what: config.what }),
      onLiveRefusal: setLiveRefusal,
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

/** The scope each call was submitted in, by the call's own variables (a copy made at submission). */
const submittedIn = new WeakMap<object, StudioScope>()

/**
 * Bind `call` to `scope`, the scope it is submitted in (the form's bound scope, #1643): the copy
 * returned is what the mutation runs. Each submission gets its own copy, so one variables object
 * submitted in two scopes is two calls.
 */
export function submitIn<V extends object>(call: V, scope: StudioScope | null): V {
  const bound = { ...call }
  if (scope !== null) submittedIn.set(bound, scope)
  return bound
}

/** Whether `error` is Studio's Worker refusing a write whose expected scope is no longer the session's. */
function workspaceChanged(error: unknown): boolean {
  if (!(error instanceof NornUnavailableError) || error.status !== 409 || error.body === null) return false
  try { return (JSON.parse(error.body) as { readonly code?: unknown } | null)?.code === "workspace.changed" } catch { return false }
}

/**
 * The mutation options of one route. A call runs in the scope it was submitted in (`submitIn`),
 * never the scope of whatever render last updated the options. Before any request it is refused
 * (`WORKSPACE_CHANGED_MESSAGE`, nothing sent) when the page's scope (`current`) or the tab's
 * displayed workspace (`getDisplayedScope`, #1643) is no longer that scope: a call queued (offline,
 * or behind another) in workspace A never starts in B. A call never bound through `submitIn` is
 * refused too. The request carries the expected-scope headers, so the Worker refuses it if the
 * session moved in another tab; that refusal is the same error. On success the submitting scope's
 * reads are invalidated.
 */
export function cellMutationOptions<P extends PostRoute>(client: QueryClient, route: P, current: () => StudioScope | null, config: {
  readonly invalidates?: (result: CellResult<P>, call: CellCall<P>) => readonly CellRoutePrefix[]
  readonly fetch?: FetchLike
}): MutationOptions<CellResult<P>, Error, CellCall<P>> {
  return {
    mutationFn: async (call) => {
      const scope = submittedIn.get(call)
      if (scope === undefined) throw new StudioScopeError(NO_DATA_CELL)
      const displayed = getDisplayedScope()
      if (!sameStudioScope(scope, current()) || (displayed !== undefined && !sameStudioScope(scope, displayed))) throw new StudioScopeError(WORKSPACE_CHANGED_MESSAGE)
      try {
        return asDocument(await (studioCellClient(scope, config.fetch).post as unknown as LooseCall)(route, call))
      } catch (error) {
        throw workspaceChanged(error) ? new StudioScopeError(WORKSPACE_CHANGED_MESSAGE) : error
      }
    },
    onSuccess: async (result, call) => {
      const scope = submittedIn.get(call)
      if (scope !== undefined && config.invalidates !== undefined) await invalidateCellRoutes(client, scope, config.invalidates(result, call))
    },
  }
}

/**
 * Call `route` in `scope`. A refusal is the typed `NornRefusalError` (code, category, checker
 * diagnostics); on success the reads `invalidates` names are read again. Each call is bound to the
 * scope it was submitted in (see {@link cellMutationOptions}).
 */
export function useCellMutation<P extends PostRoute>(route: P, config: {
  readonly scope: StudioScope | null
  readonly invalidates?: (result: CellResult<P>, call: CellCall<P>) => readonly CellRoutePrefix[]
  readonly fetch?: FetchLike
}): UseMutationResult<CellResult<P>, Error, CellCall<P>> {
  const client = useQueryClient()
  const current = useRef(config.scope)
  useLayoutEffect(() => { current.current = config.scope })
  const result = useMutation<CellResult<P>, Error, CellCall<P>>(cellMutationOptions(client, route, () => current.current, config))
  const { mutate: run, mutateAsync: runAsync } = result
  type Options = MutateOptions<CellResult<P>, Error, CellCall<P>>
  const mutate = useCallback((call: CellCall<P>, options?: Options) => run(submitIn(call, current.current), options), [run])
  const mutateAsync = useCallback((call: CellCall<P>, options?: Options) => runAsync(submitIn(call, current.current), options), [runAsync])
  return { ...result, mutate, mutateAsync }
}
