/**
 * Studio over a connected cell: which documents the pages read (the cell's, or
 * the recording when no cell is connected) and the live reads themselves. Kept
 * apart from transport.ts, which finds recordings with Vite's import.meta.glob.
 */
import { type QueryClient, useQuery, useQueryClient } from "@tanstack/react-query"
import { useEffect } from "react"
import { CellUnavailable, StudioScopeError, readStudioScope, sameStudioScope, scopedHostFetch, studioScopeKey, useStudioScope, type StudioScope } from "./definitions.ts"
import { readDocument, type Read } from "./exact.ts"
import { readsOf } from "./objects.ts"
import type { Artifact, Envelope, LinkPage, ObjectPage, ObjectRecord, SourceLineage, Transport } from "./transport.ts"
import type { CountedLinkPage, LinkCounts } from "./record-page.ts"
import { totalOf, type Total } from "./records-query.ts"

/** A read this cell does not serve. While a cell is connected nothing recorded stands in for it. */
export class NotServed extends Error {
  constructor(what: string) { super(`${what} not served by this cell yet.`) }
}

export type Fetch = (input: string, init?: RequestInit) => Promise<Response>

/** The documents under `prefix` (an RFC 6901 pointer) of one read, with their exact numbers. */
export function subRead<T>(read: Read<unknown>, prefix: string, document: T): Read<T> {
  const numbers = new Map<string, string>()
  for (const [pointer, text] of read.numbers) if (pointer === prefix || pointer.startsWith(`${prefix}/`)) numbers.set(pointer.slice(prefix.length), text)
  return { document, numbers, origin: read.origin }
}

/** The cell's link page (`next` cursor) in the shape the pages read (`hasMore`), with its counts when it sends them. */
function linkPage(read: Read<{ readonly objectType: string; readonly id: string; readonly items: LinkPage["items"]; readonly next?: string | null; readonly counts?: LinkCounts | null }>): Read<CountedLinkPage> {
  const { objectType, id, items, next, counts } = read.document
  return { ...read, document: { objectType, id, items, hasMore: typeof next === "string", ...counts ? { counts } : {} } }
}

/** Where Studio's documents come from: the connected cell, or the recording when no cell is connected. */
export type Mode =
  | {
    readonly kind: "live"
    /** Server-reported scope (`orgId` + `environmentId`) that produced this catalog and capability view. */
    readonly scope: StudioScope
    readonly catalog: Read<readonly Artifact[]>
    readonly counts: ObjectCounts
    /**
     * Named questions advertised on `GET /v1/definitions` (`publishedQuestions`, provisional
     * capability metadata until the backend lane pins published question discovery on the catalog).
     */
    readonly publishedQuestions: ReadonlySet<string>
  }
  | { readonly kind: "recorded" }

/**
 * Each object type's record count, as the cell counted it for this caller with the records list's own
 * permissions. A type the caller may not list has no entry; the cell decides, and Studio only shows it.
 */
export type ObjectCounts = Readonly<Record<string, Total>>

/** The catalog's `objectCounts`, keeping only well-formed counts. */
export function countsOf(document: unknown): ObjectCounts {
  const raw = (document as { objectCounts?: unknown } | null)?.objectCounts
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) return {}
  return Object.fromEntries(Object.entries(raw).flatMap(([objectType, total]) => {
    const counted = totalOf({ total })
    return counted ? [[objectType, counted] as const] : []
  }))
}

/**
 * Named published questions advertised on `GET /v1/definitions` (`publishedQuestions`, provisional
 * capability metadata). Fails closed to an empty set when absent or malformed.
 */
export function publishedQuestionsOf(document: unknown): ReadonlySet<string> {
  const raw = (document as { publishedQuestions?: unknown } | null)?.publishedQuestions
  if (!Array.isArray(raw)) return new Set()
  return new Set(raw.filter((name): name is string => typeof name === "string" && name !== ""))
}

/**
 * Decide once where documents come from. A workspace whose server reports a scope and whose cell
 * answers `GET /v1/definitions` is live, and every read then goes to that cell: a read it does not
 * serve says so ({@link NotServed}) and is never replaced by a recording. With no session, no cell
 * or no host connected (a 503, a 404, or a page that is not a document) Studio shows the recording,
 * and every recorded read says it is one. Any other answer is the cell's and is shown as an error.
 */
/** What the Studio Worker answers when this deployment has no Norn host configured (worker/index.ts). */
const NO_HOST = "No Norn host is connected to this deployment."

export async function readMode(
  fetcher: Fetch,
  knownScope?: StudioScope | null,
  signal?: AbortSignal,
): Promise<Mode> {
  let scope: StudioScope | null
  if (knownScope !== undefined) {
    scope = knownScope
  } else {
    try { scope = await readStudioScope(fetcher, signal) } catch (error) {
      if (signal?.aborted) throw error
      // No session server (a preview without the Worker, one that reports no scope, or no session): nothing is connected.
      // A session server that failed to answer (a network error or a 5xx) is not "nothing connected": it is an
      // outage, raised so Studio says it is reconnecting and retries, never swapping a live cell for the recording.
      if (error instanceof CellUnavailable && (error.status === 0 || error.status >= 500)) throw error
      if (error instanceof CellUnavailable || error instanceof StudioScopeError) return { kind: "recorded" }
      throw error
    }
  }
  if (scope === null) return { kind: "recorded" }
  const active = scope
  const response = await scopedHostFetch(fetcher, "/api/host", async () => active)(
    "/v1/definitions",
    { headers: { accept: "application/json" }, ...(signal === undefined ? {} : { signal }) },
  )
  const text = await response.text()
  // Only this deployment's own "no host connected" answer means there is no cell; any other 503 is a cell that
  // did not answer (a busy or restarting host), raised and retried rather than replaced by the recording.
  if (response.status === 404 || (response.status === 503 && (() => { try { return (JSON.parse(text) as { error?: unknown }).error === NO_HOST } catch { return false } })())) return { kind: "recorded" }
  let parsed: unknown
  try { parsed = JSON.parse(text) } catch { return { kind: "recorded" } }
  if (!response.ok) throw new CellUnavailable(response.status, (parsed as { message?: unknown } | null)?.message as string | undefined)
  const artifacts = (parsed as { artifacts?: unknown } | null)?.artifacts
  if (!Array.isArray(artifacts)) throw new CellUnavailable(response.status, "The cell's definitions read carried no artifacts.")
  const read = readDocument<{ readonly artifacts: readonly Artifact[] }>(text, "live")
  return {
    kind: "live",
    scope: active,
    catalog: subRead(read, "/artifacts", read.document.artifacts),
    counts: countsOf(read.document),
    publishedQuestions: publishedQuestionsOf(read.document),
  }
}

/** The live transport over one connected cell's routes. */
export function liveTransport(catalog: Read<readonly Artifact[]>, fetcher: Fetch, counts: ObjectCounts = {}, scope?: StudioScope): Transport {
  const call = scopedHostFetch(fetcher, "/api/host", scope !== undefined ? async () => scope : () => readStudioScope(fetcher))
  /** One read; `null` when the cell answers 404 (no such object, or none this principal may read). */
  const read = async <T,>(path: string): Promise<Read<T> | null> => {
    const response = await call(path, { headers: { accept: "application/json" } })
    const text = await response.text()
    if (response.status === 404) return null
    if (!response.ok) {
      let message: string | undefined
      try { message = (JSON.parse(text) as { message?: string }).message } catch { /* not a refusal document */ }
      throw new CellUnavailable(response.status, message)
    }
    return readDocument<T>(text, "live")
  }
  const objectPath = (objectType: string, id: string, leaf?: string) => `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}${leaf ? `/${leaf}` : ""}`
  return {
    catalog: async () => catalog,
    objectCounts: async () => counts,
    definitions: async () => catalog.document.map((artifact, index) => subRead(catalog, `/${index}`, artifact as Envelope)),
    questionBoard: async () => { throw new NotServed("Saved questions are") },
    restatement: async () => { throw new NotServed("Restated answers are") },
    guarantees: async () => { throw new NotServed("Model-checked guarantees are") },
    objectPage: async (objectType, after) => read<ObjectPage>(`/v1/objects/${encodeURIComponent(objectType)}${after === undefined ? "" : `?after=${encodeURIComponent(after)}`}`),
    object: async (objectType, id) => {
      const [object, links, source] = await Promise.all([
        read<ObjectRecord>(objectPath(objectType, id)),
        read<{ objectType: string; id: string; items: LinkPage["items"]; next?: string | null; counts?: LinkCounts | null }>(objectPath(objectType, id, "links")),
        // 404: no mapping produced this object (or its source is not readable): there is no lineage to show.
        read<SourceLineage>(objectPath(objectType, id, "lineage")),
      ])
      if (object === null || links === null) return null
      const page = linkPage(links)
      return readsOf(objectType, id, object.document, page.document) ? { object, links: page, source } : null
    },
  }
}

/** Raised when an in-flight mode read settles after a newer scope or `refreshMode()` superseded it. */
export class ScopeSupersededError extends Error {
  constructor() { super("The active workspace scope changed while reading cell capabilities.") }
}

let modeGeneration = 0
let activeModeScopeKey: string | null = null
const modeByScope = new Map<string, Promise<Mode>>()
let unkeyedMode: Promise<Mode> | undefined
const browserFetch: Fetch = (input, init) => fetch(input, init)

/**
 * The mode for the current server-reported scope, keyed by `orgId + environmentId`.
 * Switching from one concrete scope to another supersedes any in-flight read from the previous
 * scope so stale catalog data never caches or resolves into the new scope; a failed decision is
 * tried again on the next read.
 */
export function currentMode(
  fetcher: Fetch = browserFetch,
  knownScope?: StudioScope | null,
  signal?: AbortSignal,
): Promise<Mode> {
  if (knownScope !== undefined) {
    const key = studioScopeKey(knownScope)
    if (activeModeScopeKey !== null && activeModeScopeKey !== key) {
      modeGeneration += 1
      modeByScope.clear()
      unkeyedMode = undefined
    }
    activeModeScopeKey = key
    const existing = modeByScope.get(key)
    if (existing !== undefined) return existing
    const gen = modeGeneration
    const promise = readMode(fetcher, knownScope, signal).then(
      (result) => {
        if (modeGeneration !== gen) throw new ScopeSupersededError()
        return result
      },
      (error: unknown) => {
        if (modeGeneration === gen && modeByScope.get(key) === promise) modeByScope.delete(key)
        throw error
      },
    )
    modeByScope.set(key, promise)
    return promise
  }
  if (activeModeScopeKey !== null) {
    const existing = modeByScope.get(activeModeScopeKey)
    if (existing !== undefined) return existing
  }
  if (unkeyedMode !== undefined) return unkeyedMode
  const gen = modeGeneration
  const promise = readMode(fetcher, undefined, signal).then(
    (result) => {
      if (modeGeneration !== gen) throw new ScopeSupersededError()
      if (result.kind === "live") {
        const key = studioScopeKey(result.scope)
        if (activeModeScopeKey === null) activeModeScopeKey = key
        if (!modeByScope.has(key)) modeByScope.set(key, promise)
      }
      return result
    },
    (error: unknown) => {
      if (modeGeneration === gen && unkeyedMode === promise) unkeyedMode = undefined
      throw error
    },
  )
  unkeyedMode = promise
  return promise
}

/** A refusal the cell sent for a read: its code and message, shown as they are. */
export class CellRefusal extends Error {
  constructor(readonly status: number, readonly code: string, message: string) { super(message) }
}

/**
 * One scoped read of a cell route, with every number's exact text. A route this cell does not
 * serve (a 404 `route.not_found`, or a 503) is {@link NotServed} under `what`; any other refusal
 * is the cell's {@link CellRefusal}.
 */
export async function cellRead<T>(path: string, what: string, init: RequestInit = {}, fetcher: Fetch = browserFetch): Promise<Read<T>> {
  const call = scopedHostFetch(fetcher, "/api/host", () => readStudioScope(fetcher))
  const response = await call(path, { ...init, headers: { accept: "application/json", ...init.body === undefined ? {} : { "content-type": "application/json" }, ...init.headers } })
  const text = await response.text()
  if (response.ok) return readDocument<T>(text, "live")
  let refusal: { code?: unknown; message?: unknown } | null = null
  try { refusal = JSON.parse(text) as { code?: unknown; message?: unknown } } catch { /* not a refusal document */ }
  if (response.status === 503 || (response.status === 404 && (refusal?.code === "route.not_found" || refusal?.code === undefined))) throw new NotServed(what)
  if (typeof refusal?.code === "string") throw new CellRefusal(response.status, refusal.code, typeof refusal.message === "string" ? refusal.message : refusal.code)
  throw new CellUnavailable(response.status)
}

const lastObservedScope = new WeakMap<QueryClient, string>()

function purgeStaleScopeQueries(client: QueryClient, nextKey: string): void {
  const isStale = (query: { readonly queryKey: readonly unknown[] }) => {
    const head = query.queryKey[0]
    if (head === "session" || head === "studio-scope") return false
    if (head === "studio-mode" && query.queryKey[1] === nextKey) return false
    return true
  }
  void client.cancelQueries({ predicate: isStale })
  client.removeQueries({ predicate: isStale })
}

function maybeInvalidateSession(client: QueryClient, nextScope: StudioScope | null): void {
  const cached = client.getQueryData<{
    readonly organization?: { readonly id?: string } | null
    readonly environmentId?: string | null
  } | null>(["session"])
  if (cached === undefined) return
  const cachedScope: StudioScope | null = cached !== null && cached.organization?.id && cached.environmentId
    ? { orgId: cached.organization.id, environmentId: cached.environmentId }
    : null
  if (!sameStudioScope(cachedScope, nextScope)) {
    void client.invalidateQueries({ queryKey: ["session"] })
  }
}

/**
 * Synchronize Studio's client caches when a scoped read discovers `/api/me` moved to a new scope
 * (org or environment): invalidates the old mode/catalog, cancels and drops old-scope queries,
 * updates `["studio-scope"]` so every capability gate closes immediately until the new scope's
 * catalog arrives, and refreshes the session query if needed.
 */
export function syncStudioScopeChange(client: QueryClient, nextScope: StudioScope | null): void {
  const nextKey = studioScopeKey(nextScope)
  lastObservedScope.set(client, nextKey)
  if (activeModeScopeKey !== nextKey) refreshMode()
  purgeStaleScopeQueries(client, nextKey)
  client.setQueryData(["studio-scope"], nextScope)
  maybeInvalidateSession(client, nextScope)
}

/**
 * The mode for the current server-reported scope (`orgId` + `environmentId`), retried while the
 * session server or cell does not answer (network or 5xx, backing off to 8 s). Keyed by scope so
 * an environment switch in the same tab immediately invalidates the previous catalog and closes
 * capability gates until the new scope's `/v1/definitions` arrives.
 */
export function useMode() {
  const client = useQueryClient()
  const scope = useStudioScope()
  const scopeOutage = scope.isError && scope.error instanceof CellUnavailable && (scope.error.status === 0 || scope.error.status >= 500)
  const scopeReady = !scope.isPending
  const activeScope = scope.isSuccess ? scope.data : null
  const key = scopeOutage ? "outage" : studioScopeKey(activeScope)

  useEffect(() => {
    if (!scope.isSuccess) return
    const prev = lastObservedScope.get(client)
    lastObservedScope.set(client, key)
    if (prev !== undefined && prev !== key) {
      if (activeModeScopeKey !== key) refreshMode()
      purgeStaleScopeQueries(client, key)
      maybeInvalidateSession(client, activeScope)
    }
  }, [client, scope.isSuccess, key, activeScope])

  return useQuery({
    queryKey: ["studio-mode", key],
    enabled: scopeReady,
    queryFn: async ({ signal }) => {
      if (!scopeOutage) return currentMode(browserFetch, activeScope, signal)
      const recovered = await readMode(browserFetch, undefined, signal)
      client.setQueryData(["studio-scope"], recovered.kind === "live" ? recovered.scope : null)
      return recovered
    },
    retry: (failureCount, error) =>
      failureCount < 6
      && !(error instanceof ScopeSupersededError)
      && !(error instanceof CellUnavailable && error.status >= 400 && error.status < 500),
    retryDelay: (attempt) => Math.min(1000 * 2 ** attempt, 8000),
    staleTime: Infinity,
  })
}

/** Forget the decided mode, so the next read takes the cell's current catalog (after a load installs a new type or scope changes). */
export function refreshMode(): void {
  modeGeneration += 1
  activeModeScopeKey = null
  modeByScope.clear()
  unkeyedMode = undefined
}
