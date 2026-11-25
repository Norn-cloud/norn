import { hashKey, queryOptions, replaceEqualDeep, useMutation, useQuery, useQueryClient, type MutateOptions, type MutationOptions, type QueryClient, type QueryKey, type UseMutationResult, type UseQueryResult } from "@tanstack/react-query"
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react"
import {
  createNornClient, NornUnavailable,
  type ExactDocument, type Fetch, type GetRoute, type LiveRefusal, type LiveSnapshot, type NornClient, type Operation, type PostRoute,
  type Refusal, type RouteCall, type RouteResult, type Success,
} from "../../../../packages/norn-sdk/src/index.ts"
import { CellUnavailable, defaultFetch, getDisplayedScope, NO_DATA_CELL, parseUnavailableDetail, sameStudioScope, scopedHostFetch, StudioScopeError, WORKSPACE_CHANGED_MESSAGE, type StudioScope } from "./definitions.ts"
import { live } from "./live-reads.ts"
import { CellRefusal, NotServed } from "./live-transport.ts"

/**
 * One read primitive for the cell's documents (#1374): `useCellQuery(route, params, { scope })` is
 * a TanStack query over the `@norn/sdk` client joined to the page's live socket. The cell pushes a
 * changed answer and the pushed document is written into the cache (`setQueryData`): nothing is
 * read again and nothing polls. Studio renders what the cell sends and decides nothing.
 *
 * - The key is `[route, orgId, environmentId, params]`: reads of two scopes never share an entry.
 * - Documents are exact (`ExactDocument`): every number is the text the cell sent.
 * - A refusal is the existing refusal state: a {@link CellRefusal}, {@link CellUnavailable} or
 *   {@link NotServed} error. A read's refusal is the query's error. A pushed refusal is the read's
 *   data, a {@link CellRefused} entry, so nothing TanStack does to the query's error state (a
 *   retry, a cancelled fetch's revert on unmount) can bring the refused document back; pages
 *   render {@link cellReadState}.
 * - Unmounting unsubscribes; the live client keeps its own limits (frame size, one wire
 *   subscription per path, the cell's `live.too_many_subscriptions`).
 * - A read the cell's answers no longer reach is stale (`CellReadState.stale`): its subscription
 *   was refused, or the live client gave up reconnecting (`LIVE_UNAVAILABLE`). The page shows the
 *   last answer as such, with its time and a way to read again, never as live.
 * - Nothing is kept across reloads. Persistence lands with the server's cache-policy flag (#1374),
 *   keyed by reader and scope.
 */

/** A cell document as the cache holds it. */
export interface CellDocument<T> {
  readonly kind: "document"
  readonly status: number
  readonly document: ExactDocument<T>
  /** The live digest of a pushed document; null for a read. */
  readonly digest: string | null
}

/**
 * A refusal the cell pushed for the read, held as the read's data in place of its document: the
 * refusal state it maps to, and the live sequence of the frame that carried it.
 */
export interface CellRefused {
  readonly kind: "refused"
  readonly refusal: Error
  readonly seq: number
}

/** What the cache holds for one read: the cell's document, or the refusal it pushed instead. */
export type CellEntry<T> = CellDocument<T> | CellRefused

/** The parameters of one read: its path parameters and query, without the scope. */
export type CellParams<P extends GetRoute> = Pick<RouteCall<Operation<P, "get">>, "path" | "query">
export type CellData<P extends GetRoute> = CellEntry<Success<Operation<P, "get">>>
export type CellQueryKey = readonly [route: string, orgId: string, environmentId: string, params: unknown]

const NO_SCOPE: StudioScope = { orgId: "", environmentId: "" }

export const cellQueryKey = (route: string, scope: StudioScope, params: unknown): CellQueryKey =>
  [route, scope.orgId, scope.environmentId, params ?? {}]

/**
 * The `@norn/sdk` client for `scope` through Studio's host proxy, as the signed-in account
 * (`scopedHostFetch`): reads keep Studio's read deadline, and a write carries the expected-scope
 * headers (#1643), so Studio's Worker refuses it (`409 workspace.changed`) if the session's active
 * workspace is no longer `scope`.
 */
export const studioCellClient = (scope: StudioScope, fetcher: Fetch = defaultFetch): NornClient =>
  createNornClient({ scope, fetch: scopedHostFetch(fetcher, "/api/host", async () => scope) })

/** The scoped path a live subscription names, as the GET asks it. */
export const cellPath = <P extends GetRoute>(route: P, scope: StudioScope, params: CellParams<P>): string =>
  (studioCellClient(scope).path as (route: string, call?: object) => string)(route, params)

/** A refusal the cell answered a read with, over HTTP or pushed, as the existing refusal state. */
function refusalState(refusal: Refusal): Error {
  const detail = parseUnavailableDetail(refusal)
  if (refusal.status === 503) return new CellUnavailable(503, refusal.message === "" ? undefined : refusal.message, detail)
  return new CellRefusal(refusal.status, refusal.code, refusal.message === "" ? refusal.code : refusal.message, detail)
}

/**
 * The SDK's "no readable answer" (`NornUnavailable`) as the existing state: a route the cell does
 * not serve (a 404 or 503 that is no refusal, as Studio's Worker answers with no host), or a cell
 * that did not answer. No code and no detail are read from it. Any other error passes as is.
 */
function unansweredState(error: unknown, what: string): unknown {
  if (!(error instanceof NornUnavailable)) return error
  if (error.notServed) return new NotServed(what)
  return new CellUnavailable(error.status, undefined, error.status === 0 ? { retryable: true } : {})
}

/**
 * The refusal state for an answer the cell pushed with a failing status. The SDK's live client
 * checks the body as a route's HTTP answer is checked (`routeRefusalOf`): a pushed body that is no
 * Refusal@2 has no `refusal`, so it is unavailable (or not served), never read for a code.
 */
export const pushedRefusal = (snapshot: Pick<LiveSnapshot, "status" | "refusal">, what: string): Error =>
  snapshot.refusal !== undefined ? refusalState(snapshot.refusal) : unansweredState(new NornUnavailable(snapshot.status), what) as Error

type LooseCall = (route: string, call?: object) => Promise<RouteResult<unknown>>

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
  readonly fetch?: Fetch
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
      // A frame the cell pushed while this read was in flight (a document or a refusal) is at least
      // as new as the read's answer.
      const pushedSince = (): CellData<P> | undefined =>
        sequenceOf(client, hash) === started ? undefined : client.getQueryData<CellData<P>>(queryKey)
      let read: RouteResult<unknown>
      try {
        read = await (studioCellClient(config.scope, config.fetch).get as LooseCall)(route, { ...params, signal })
      } catch (error) {
        const pushed = pushedSince()
        if (pushed !== undefined) return pushed
        throw unansweredState(error, what)
      }
      const pushed = pushedSince()
      if (pushed !== undefined) return pushed
      if (read.kind === "refused") throw refusalState(read)
      return { kind: "document", status: read.status, document: read.document as ExactDocument<Success<Operation<P, "get">>>, digest: null }
    },
  })
}

/** The slice of the live client a read listens through (`LiveClient` is one). */
export interface LiveSource {
  subscribe(path: string, listener: (snapshot: LiveSnapshot) => void, options?: { readonly onRefused?: (refusal: LiveRefusal) => void }): () => void
}

/**
 * Join the read under `queryKey` to the cell's pushes for `path`: a changed answer is written into
 * the cache, a pushed refusal replaces the read's data with a {@link CellRefused} entry. Returns
 * the unsubscribe.
 *
 * - Both are written with `setQueryData`, so each becomes the state TanStack restores if the read
 *   in flight is cancelled (the last observer unmounting): a cancelled read never brings back the
 *   document a later push replaced or refused. The read itself is fenced by the live sequence
 *   (`cellQueryOptions`), so its answer never lands over the push either. Nothing is cleared and
 *   nothing relies on the query's error state.
 * - An unchanged answer is not written again when the cache already shows that document as a
 *   success. After a failed read (TanStack keeps the old document beside the error) the same
 *   answer is written, so the cell's confirmation clears the error.
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
      const state = client.getQueryState<CellEntry<unknown>, Error>(queryKey)
      const current = state?.data
      // An unchanged answer (the first frame after subscribing is the page's own read) keeps the
      // cached document as it is, but only when the cache shows it as a success; the sequence above
      // still fences any read in flight. The exact body holds every number's text, so equal bodies
      // are equal to the digit.
      if (state?.status === "success" && current?.kind === "document" && replaceEqualDeep(current.document, snapshot.exact) === current.document) return
      const pushed: CellDocument<unknown> = { kind: "document", status: snapshot.status, document: snapshot.exact, digest: snapshot.digest }
      client.setQueryData(queryKey, pushed)
      return
    }
    const refused: CellRefused = { kind: "refused", refusal: pushedRefusal(snapshot, options.what ?? route), seq: sequenceOf(client, hash) }
    client.setQueryData(queryKey, refused)
  }, onLiveRefusal ? { onRefused: onLiveRefusal } : undefined)
  replaying = false
  return stop
}

/** What a page renders for one read: the document, or the refusal state (the read's error or a pushed refusal). */
export interface CellReadState<T> {
  readonly status: "pending" | "error" | "success"
  readonly data: CellDocument<T> | undefined
  readonly error: Error | null
  /**
   * The cell's answers to this read are no longer followed: the live protocol refused its
   * subscription, or the live client gave up reconnecting (`LIVE_UNAVAILABLE`). `data` is the last
   * answer read (as of the query's `dataUpdatedAt`); the page says so and offers to read again
   * (`refetch`), never shows it as live. It clears when the cell answers the subscription again.
   */
  readonly stale: boolean
}

/**
 * The read as a page shows it. A pushed refusal is the error state with no document; a later
 * failed read of it shows that failure. Otherwise the query's own status, document and error.
 * `liveRefusal` is the live protocol's refusal of the read's subscription, if any.
 */
export function cellReadState<T>(read: Pick<UseQueryResult<CellEntry<T>, Error>, "status" | "data" | "error">, liveRefusal: LiveRefusal | null = null): CellReadState<T> {
  const entry = read.data
  const stale = liveRefusal !== null
  if (entry?.kind === "refused") return { status: "error", data: undefined, error: read.status === "error" ? read.error : entry.refusal, stale }
  return { status: read.status, data: entry, error: read.error, stale }
}

export interface CellQuery<T> {
  /** The query itself: its data is the cache entry, a document or a pushed refusal. Render {@link CellQuery.state}. */
  readonly read: UseQueryResult<CellEntry<T>, Error>
  readonly state: CellReadState<T>
  /** The live protocol's refusal of this read's subscription (`live.too_many_subscriptions`, `live.unavailable`, …): the read stands, without pushes, and `state.stale` is set. */
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
  return { read, state: cellReadState(read, liveRefusal), liveRefusal }
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
export type CellCall<P extends PostRoute> = Pick<RouteCall<Operation<P, "post">>, "path" | "query" | "body">
/** What the cell answered a call with: its document, or its refusal (an answer, as the SDK reads it, not an error). */
export type CellResult<P extends PostRoute> = RouteResult<Success<Operation<P, "post">>>
/** A call the cell answered with its document. */
export type CellWritten<P extends PostRoute> = Extract<CellResult<P>, { readonly kind: "document" }>

/**
 * One call as it was submitted, fixed then and never changed: the operation (route and method), a
 * frozen copy of its path parameters, query and body, the scope it was submitted in (the form's
 * bound scope, #1643), its idempotency key, and the reads it invalidates. This is the mutation's
 * variables, and the mutation runs exactly this: TanStack swaps a pending mutation's options on
 * every render, so nothing about what a queued call does may come from them.
 */
export interface CellSubmission<P extends PostRoute> {
  readonly route: P
  readonly method: "POST"
  readonly call: CellCall<P>
  readonly scope: StudioScope | null
  /** The body's `idempotencyKey`, when it carries one: the cell commits a retried call once. */
  readonly idempotencyKey: string | null
  readonly invalidates: ((result: CellWritten<P>) => readonly CellRoutePrefix[]) | null
}

/** Every submission `submitCall` made: a variables object built any other way never runs. */
const submissions = new WeakSet<object>()

/** `value`, frozen all the way down. */
function frozen<T>(value: T): T {
  if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value)
    for (const member of Object.values(value)) frozen(member)
  }
  return value
}

/**
 * Submit `call` to `route` in `scope`: the immutable {@link CellSubmission} the mutation runs. The
 * call is deep-copied and frozen, so changing the caller's object afterwards changes nothing, and
 * each submission is its own call (one variables object submitted in two scopes is two calls).
 */
export function submitCall<P extends PostRoute>(route: P, call: CellCall<P>, scope: StudioScope | null, invalidates?: (result: CellWritten<P>, call: CellCall<P>) => readonly CellRoutePrefix[]): CellSubmission<P> {
  const copy = frozen(structuredClone({ path: call.path, query: call.query, body: call.body })) as CellCall<P>
  const key = (copy.body as { readonly idempotencyKey?: unknown } | undefined)?.idempotencyKey
  const submission: CellSubmission<P> = Object.freeze({
    route,
    method: "POST" as const,
    call: copy,
    scope: scope === null ? null : Object.freeze({ orgId: scope.orgId, environmentId: scope.environmentId }),
    idempotencyKey: typeof key === "string" ? key : null,
    invalidates: invalidates === undefined ? null : (result: CellWritten<P>) => invalidates(result, copy),
  })
  submissions.add(submission)
  return submission
}

/**
 * The mutation options cell writes run under. They name no route: the mutation function runs the
 * {@link CellSubmission} it is handed, its route, call, scope and invalidation, and nothing from
 * the render that last updated the options. Before any request a submission is refused
 * (`WORKSPACE_CHANGED_MESSAGE`, nothing sent) when the page's scope (`current`) or the tab's
 * displayed workspace (`getDisplayedScope`, #1643) is no longer the one it was submitted in: a call
 * queued (offline, or behind another) in workspace A never starts in B. A variables object
 * `submitCall` did not make is refused too. The request carries the expected-scope headers, so the
 * Worker refuses it (`409 workspace.changed`) if the session moved in another tab; that refusal is
 * the same error. Any other refusal is the call's answer. On a document the submission's own
 * invalidation runs in its own scope.
 */
export function cellMutationOptions<P extends PostRoute>(client: QueryClient, current: () => StudioScope | null, config: {
  readonly fetch?: Fetch
} = {}): MutationOptions<CellResult<P>, Error, CellSubmission<P>> {
  return {
    mutationFn: async (submission) => {
      if (!submissions.has(submission) || submission.scope === null) throw new StudioScopeError(NO_DATA_CELL)
      const { scope } = submission
      const displayed = getDisplayedScope()
      if (!sameStudioScope(scope, current()) || (displayed !== undefined && !sameStudioScope(scope, displayed))) throw new StudioScopeError(WORKSPACE_CHANGED_MESSAGE)
      const result = await (studioCellClient(scope, config.fetch).post as LooseCall)(submission.route, submission.call) as CellResult<P>
      if (result.kind === "refused" && result.status === 409 && result.code === "workspace.changed") throw new StudioScopeError(WORKSPACE_CHANGED_MESSAGE)
      return result
    },
    onSuccess: async (result, submission) => {
      if (result.kind === "document" && submissions.has(submission) && submission.scope !== null && submission.invalidates !== null) {
        await invalidateCellRoutes(client, submission.scope, submission.invalidates(result as CellWritten<P>))
      }
    },
  }
}

type CellMutateOptions<P extends PostRoute> = MutateOptions<CellResult<P>, Error, CellSubmission<P>>

/** {@link useCellMutation}'s result: TanStack's, with `mutate` taking the call and submitting it. */
export type CellMutation<P extends PostRoute> = Omit<UseMutationResult<CellResult<P>, Error, CellSubmission<P>>, "mutate" | "mutateAsync"> & {
  readonly mutate: (call: CellCall<P>, options?: CellMutateOptions<P>) => void
  readonly mutateAsync: (call: CellCall<P>, options?: CellMutateOptions<P>) => Promise<CellResult<P>>
}

/**
 * Call `route` in `scope`. The answer is the cell's document or its typed refusal (code,
 * category, checker diagnostics at their pointers); a lost answer is the SDK's `NornUnavailable`,
 * whose `outcome` says whether the call may have been applied. On a document the reads
 * `invalidates` names are read again. Each call is submitted (`submitCall`) when `mutate` is
 * called, with the route, scope and invalidation of that render: a call queued while the page
 * later renders another route or scope still runs as submitted (see {@link cellMutationOptions}).
 */
export function useCellMutation<P extends PostRoute>(route: P, config: {
  readonly scope: StudioScope | null
  readonly invalidates?: (result: CellWritten<P>, call: CellCall<P>) => readonly CellRoutePrefix[]
  readonly fetch?: Fetch
}): CellMutation<P> {
  const client = useQueryClient()
  const latest = useRef({ route, scope: config.scope, invalidates: config.invalidates })
  useLayoutEffect(() => { latest.current = { route, scope: config.scope, invalidates: config.invalidates } })
  const result = useMutation<CellResult<P>, Error, CellSubmission<P>>(cellMutationOptions<P>(client, () => latest.current.scope, config.fetch === undefined ? {} : { fetch: config.fetch }))
  const { mutate: run, mutateAsync: runAsync } = result
  const submit = useCallback((call: CellCall<P>) => submitCall(latest.current.route, call, latest.current.scope, latest.current.invalidates), [])
  const mutate = useCallback((call: CellCall<P>, options?: CellMutateOptions<P>) => run(submit(call), options), [run, submit])
  const mutateAsync = useCallback((call: CellCall<P>, options?: CellMutateOptions<P>) => runAsync(submit(call), options), [runAsync, submit])
  return { ...result, mutate, mutateAsync }
}
