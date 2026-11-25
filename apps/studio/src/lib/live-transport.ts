/**
 * Studio over a connected cell: which documents the pages read (the cell's, or
 * the recording when no cell is connected) and the live reads themselves. Kept
 * apart from transport.ts, which finds recordings with Vite's import.meta.glob.
 */
import { queryOptions, type QueryClient, useQuery, useQueryClient } from "@tanstack/react-query"
import { useEffect } from "react"
import { CellUnavailable, StudioScopeError, defaultFetch, isReadMethod, parseUnavailableDetail, readAtScope, readStudioScope, recordCellReachable, resolveWriteScope, sameStudioScope, scopedHostFetch, studioScopeKey, useStudioScope, type ScopedRequestInit, type StudioScope, type UnavailableDetail } from "./definitions.ts"
import { NO_HOST_ERROR } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"
import { readsOf } from "./record-name.ts"
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

async function discoverScope(fetcher: Fetch, signal?: AbortSignal): Promise<StudioScope | null> {
  try { return await readStudioScope(fetcher, signal) } catch (error) {
    signal?.throwIfAborted()
    // A session server outage must retry; a missing session or scope can use the recording.
    if (error instanceof CellUnavailable && (error.status === 0 || error.status >= 500)) throw error
    if (error instanceof CellUnavailable || error instanceof StudioScopeError) return null
    throw error
  }
}

/**
 * Where the read behind a mode or error actually went: the scope (`studioScopeKey`) its
 * `/v1/definitions` request was sent to, or `null` when the session server's own `/api/me` discovery
 * was all it reached (it failed, or named no workspace), plus the mode generation the read started in.
 * A read that outlives `refreshMode` (for example, one waiting in discovery) stays its starting
 * generation's, so it neither records a settle for the next generation nor clears one.
 * Each mode and each error `readMode` produces carries it, so whatever later records that outcome (the
 * mode query's settled failure) binds it to what answered, never to whichever scope happens to be
 * active when it is stored. A read aborted by its caller carries none.
 */
type DefinitionsTarget = { readonly scopeKey: string | null; readonly generation: number }
const definitionsTargets = new WeakMap<object, DefinitionsTarget>()

/** Where the read behind this mode or error went ({@link DefinitionsTarget}), if it was not aborted. */
function definitionsTargetOf(outcome: unknown): DefinitionsTarget | undefined {
  return typeof outcome === "object" && outcome !== null ? definitionsTargets.get(outcome) : undefined
}

function bindTarget(outcome: unknown, target: DefinitionsTarget, signal?: AbortSignal): void {
  if (signal?.aborted !== true && typeof outcome === "object" && outcome !== null) definitionsTargets.set(outcome, target)
}

export async function readMode(
  fetcher: Fetch,
  knownScope?: StudioScope | null,
  signal?: AbortSignal,
): Promise<Mode> {
  signal?.throwIfAborted()
  const generation = modeGeneration
  let scope: StudioScope | null = knownScope ?? null
  let session: DefinitionsTarget | undefined
  if (knownScope === undefined) {
    session = { scopeKey: null, generation }
    try { scope = await discoverScope(fetcher, signal) } catch (error) {
      bindTarget(error, session, signal)
      throw error
    }
  }
  signal?.throwIfAborted()
  if (scope === null) {
    const recorded: Mode = { kind: "recorded" }
    if (session !== undefined) bindTarget(recorded, session)
    return recorded
  }
  const target: DefinitionsTarget = { scopeKey: studioScopeKey(scope), generation }
  try {
    const mode = await readDefinitions(fetcher, scope, signal)
    bindTarget(mode, target)
    return mode
  } catch (error) {
    bindTarget(error, target, signal)
    throw error
  }
}

async function readDefinitions(fetcher: Fetch, scope: StudioScope, signal?: AbortSignal): Promise<Mode> {
  let response: Response
  let text: string
  try {
    response = await scopedHostFetch(fetcher, "/api/host", async () => scope)(
      "/v1/definitions",
      { headers: { accept: "application/json" }, ...(signal === undefined ? {} : { signal }) },
    )
    signal?.throwIfAborted()
    text = await response.text()
    signal?.throwIfAborted()
  } catch (error) {
    signal?.throwIfAborted()
    if (error instanceof Error && error.name === "AbortError") throw error
    if (error instanceof CellUnavailable || error instanceof StudioScopeError) throw error
    throw new CellUnavailable(0, undefined, { retryable: true })
  }
  // Only 404 or this deployment's own "no host connected" 503 means there is no cell; any other
  // non-OK answer (including HTML 502/503/504 gateway errors or cell refusals) is raised so Studio
  // retries transient outages or shows the refusal rather than swapping a live cell for the recording.
  if (response.status === 404) return { kind: "recorded" }
  let parsed: unknown = null
  let isJson = false
  try { parsed = JSON.parse(text); isJson = true } catch { /* non-JSON body */ }
  if (response.status === 503 && isJson && (parsed as { error?: unknown } | null)?.error === NO_HOST_ERROR) {
    return { kind: "recorded" }
  }
  if (!response.ok) {
    const raw = isJson ? (parsed as { message?: unknown } | null) : null
    const message = typeof raw?.message === "string" && raw.message !== "" ? raw.message : undefined
    throw new CellUnavailable(response.status, message, isJson ? parseUnavailableDetail(parsed) : {})
  }
  // A 200 HTML page (static SPA fallback with no Worker) means no live cell is connected.
  if (!isJson) return { kind: "recorded" }
  const artifacts = (parsed as { artifacts?: unknown } | null)?.artifacts
  if (!Array.isArray(artifacts)) throw new CellUnavailable(response.status, "The cell's definitions read carried no artifacts.")
  recordCellReachable(scope)
  const read = readDocument<{ readonly artifacts: readonly Artifact[] }>(text, "live")
  return {
    kind: "live",
    scope,
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
      let parsed: unknown = null
      try { parsed = JSON.parse(text) } catch { /* not a refusal document */ }
      const message = typeof (parsed as { message?: unknown } | null)?.message === "string" ? (parsed as { message: string }).message : undefined
      throw new CellUnavailable(response.status, message, parseUnavailableDetail(parsed))
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
    guarantees: async () => { throw new NotServed("Model-checked proofs are") },
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

/**
 * Read one live record, its links, and its lineage pinned explicitly to `scope`. Verifies `/api/me`
 * matches `scope` before and after dispatch so a stale drawer cannot read from another workspace.
 */
export async function readObjectAtScope(
  objectType: string,
  id: string,
  scope: StudioScope,
  signal?: AbortSignal,
  fetcher: Fetch = defaultFetch,
) {
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
  const objectPath = (leaf?: string) => `/v1/objects/${encodeURIComponent(objectType)}/${encodeURIComponent(id)}${leaf ? `/${leaf}` : ""}`
  return readAtScope(
    scope,
    (s) => readStudioScope(fetcher, s),
    async () => {
      const [object, links, source] = await Promise.all([
        read<ObjectRecord>(objectPath()),
        read<{ objectType: string; id: string; items: LinkPage["items"]; next?: string | null; counts?: LinkCounts | null }>(objectPath("links")),
        read<SourceLineage>(objectPath("lineage")),
      ])
      if (object === null || links === null) return null
      const page = linkPage(links)
      return readsOf(objectType, id, object.document, page.document) ? { object, links: page, source } : null
    },
    signal,
  )
}

/** Raised when an in-flight mode read settles after a newer scope or `refreshMode()` superseded it. */
export class ScopeSupersededError extends Error {
  constructor() { super("The active workspace scope changed while reading cell capabilities.") }
}

let modeGeneration = 0
let activeModeScopeKey: string | null = null
type ModeProbe = { readonly controller: AbortController; readonly result: Promise<Mode> }
const modeByScope = new Map<string, ModeProbe>()
let unkeyedMode: ModeProbe | undefined
/**
 * The error the mode query settled on once its bounded retries ran out (or a non-retryable refusal),
 * bound to where its read went ({@link DefinitionsTarget}) in the current mode generation. It is recorded
 * when that scope is the active one, or when no workspace is active (outage recovery before any scope
 * is known): then it is bound either to the workspace recovery discovered and was refused by, or, when
 * `/api/me` itself never answered, to no workspace at all (a settled session outage).
 *
 * While it holds, page reads through the transport reuse it instead of starting their own
 * `/v1/definitions` probe, so a settled "Workspace unavailable" issues no further automatic catalog reads:
 * - bound to the active workspace: rejected at once, with no `/api/me` or definitions read;
 * - bound to a workspace while none is active: rejected only after the reader's own `/api/me` discovery
 *   returns that same workspace (one `/api/me` read, no definitions read);
 * - a session outage while none is active: rejected at once, with no `/api/me` or definitions read.
 *
 * It clears on Retry (`retryMode` → `refreshMode`), on any keyed scope switch, and on a mode read that
 * succeeds in its generation for its workspace (or, for a session outage, any such read: the session
 * server answered it). The two records stored while no workspace is active also clear as soon as the
 * page's scope query answers ({@link scopeAnswered}), whichever workspace it names: the page then shows
 * that workspace's own mode query, which may read nothing when its mode is cached. A read that resolves
 * to another scope or generation never sees a record (no refusal detail, such as an incident ID, crosses
 * workspaces).
 */
type SettledModeFailure = { readonly error: unknown } & DefinitionsTarget
let settledModeFailure: SettledModeFailure | undefined

/**
 * The settled failure a read for `scopeKey` reuses (`null`: no workspace is active or known yet). A
 * record for another scope or generation is cleared, except that a workspace-bound record is kept for
 * a `null` reader: whose it is can only be told after that reader's own discovery.
 */
function settledFailureFor(scopeKey: string | null): SettledModeFailure | undefined {
  const settled = settledModeFailure
  if (settled === undefined) return undefined
  if (settled.generation === modeGeneration && settled.scopeKey === scopeKey) return settled
  if (settled.generation === modeGeneration && scopeKey === null) return undefined
  settledModeFailure = undefined
  return undefined
}
const pendingModeControllers = new Set<AbortController>()
const browserFetch: Fetch = (input, init) => fetch(input, init)

function startModeProbe(read: (signal: AbortSignal) => Promise<Mode>): ModeProbe {
  const controller = new AbortController()
  pendingModeControllers.add(controller)
  const result = read(controller.signal).finally(() => { pendingModeControllers.delete(controller) })
  return { controller, result }
}

function observeModeProbe(probe: ModeProbe, signal?: AbortSignal): Promise<Mode> {
  if (signal === undefined) return probe.result
  const abort = () => probe.controller.abort(signal.reason)
  if (signal.aborted) abort()
  else signal.addEventListener("abort", abort, { once: true })
  return probe.result.finally(() => signal.removeEventListener("abort", abort))
}

/**
 * The mode for the current server-reported scope, keyed by `orgId + environmentId`.
 * Switching from one concrete scope to another supersedes any in-flight read from the previous
 * scope so stale catalog data never caches or resolves into the new scope; a failed decision is
 * tried again on the next read.
 */
export function currentMode(
  fetcher: Fetch = defaultFetch,
  knownScope?: StudioScope | null,
  signal?: AbortSignal,
): Promise<Mode> {
  if (knownScope !== undefined) {
    const key = studioScopeKey(knownScope)
    if (activeModeScopeKey !== null && activeModeScopeKey !== key) {
      modeGeneration += 1
      modeByScope.clear()
    }
    if (activeModeScopeKey !== key) settledModeFailure = undefined
    unkeyedMode = undefined
    activeModeScopeKey = key
    const existing = modeByScope.get(key)
    if (existing !== undefined && !existing.controller.signal.aborted) return observeModeProbe(existing, signal)
    const gen = modeGeneration
    const probe = startModeProbe((probeSignal) => readMode(fetcher, knownScope, probeSignal).then(
      (result) => {
        if (modeGeneration !== gen) throw new ScopeSupersededError()
        return result
      },
      (error: unknown) => {
        if (modeByScope.get(key) === probe) modeByScope.delete(key)
        throw error
      },
    ))
    modeByScope.set(key, probe)
    return observeModeProbe(probe, signal)
  }
  if (activeModeScopeKey !== null) {
    const existing = modeByScope.get(activeModeScopeKey)
    if (existing !== undefined && !existing.controller.signal.aborted) return observeModeProbe(existing, signal)
  }
  if (unkeyedMode !== undefined && !unkeyedMode.controller.signal.aborted) return observeModeProbe(unkeyedMode, signal)
  const settled = settledFailureFor(activeModeScopeKey)
  if (settled !== undefined) return Promise.reject(settled.error)
  const gen = modeGeneration
  const probe = startModeProbe((probeSignal) => discoverScope(fetcher, probeSignal).then(async (scope) => {
    const key = studioScopeKey(scope)
    const checkScope = () => {
      if (modeGeneration !== gen || (activeModeScopeKey !== null && activeModeScopeKey !== key)) {
        throw new ScopeSupersededError()
      }
    }
    checkScope()
    const existing = modeByScope.get(key)
    const shared = existing !== undefined && !existing.controller.signal.aborted
    if (!shared) {
      // Outage recovery settled on the workspace this read discovered: reuse that, never re-read it.
      const settled = settledFailureFor(key)
      if (settled !== undefined) throw settled.error
      modeByScope.set(key, probe)
    }
    try {
      const result = await (shared ? observeModeProbe(existing, probeSignal) : readMode(fetcher, scope, probeSignal))
      checkScope()
      if (result.kind === "live") activeModeScopeKey = key
      return result
    } catch (error) {
      if (modeByScope.get(key) === probe) modeByScope.delete(key)
      throw error
    }
  }).catch((error: unknown) => {
    if (unkeyedMode === probe) unkeyedMode = undefined
    throw error
  }))
  unkeyedMode = probe
  return observeModeProbe(probe, signal)
}

/** A refusal the cell sent for a read: its code, message, and structured detail, shown honestly. */
export class CellRefusal extends Error {
  readonly explanation: string | undefined
  readonly retryable: boolean | undefined
  readonly incidentId: string | undefined
  readonly category: string | undefined
  readonly decidedAt: string | undefined

  constructor(readonly status: number, readonly code: string, message: string, detail: UnavailableDetail = {}) {
    super(message)
    this.explanation = detail.explanation
    this.retryable = detail.retryable
    this.incidentId = detail.incidentId
    this.category = detail.category
    this.decidedAt = detail.decidedAt
  }
}

/**
 * One scoped call to a cell route, with every number's exact text. A cell-level 503 on a read
 * preserves its refusal details as {@link CellUnavailable}. Writes retain their existing 503
 * handling as {@link NotServed}, including the dataset fallback to governed records, and bind
 * to the originating scope via {@link resolveWriteScope}.
 */
export async function cellRead<T>(
  path: string,
  what: string,
  init: ScopedRequestInit = {},
  fetcher: Fetch = defaultFetch,
  boundScope?: StudioScope | null,
): Promise<Read<T>> {
  const write = !isReadMethod(init.method)
  const scopeProvider = write
    ? () => resolveWriteScope(fetcher, boundScope)
    : boundScope !== undefined
      ? async () => boundScope
      : () => readStudioScope(fetcher, init.signal ?? undefined)
  const call = scopedHostFetch(fetcher, "/api/host", scopeProvider)
  const response = await call(path, { ...init, headers: { accept: "application/json", ...init.body === undefined ? {} : { "content-type": "application/json" }, ...init.headers } })
  const text = await response.text()
  if (response.ok) return readDocument<T>(text, "live")
  let refusal: { code?: unknown; message?: unknown; error?: unknown } | null = null
  try { refusal = JSON.parse(text) as { code?: unknown; message?: unknown; error?: unknown } } catch { /* not a refusal document */ }
  const read = isReadMethod(init.method)
  if (response.status === 404 && (refusal?.code === "route.not_found" || refusal?.code === undefined)) throw new NotServed(what)
  if (response.status === 503 && (!read || refusal?.error === NO_HOST_ERROR)) throw new NotServed(what)
  const detail = read ? parseUnavailableDetail(refusal) : undefined
  if (response.status === 503) {
    const message = typeof refusal?.message === "string" && refusal.message !== "" ? refusal.message : undefined
    throw new CellUnavailable(response.status, message, detail)
  }
  if (typeof refusal?.code === "string") {
    throw new CellRefusal(response.status, refusal.code, typeof refusal.message === "string" ? refusal.message : refusal.code, detail)
  }
  throw new CellUnavailable(response.status, read && typeof refusal?.message === "string" ? refusal.message : undefined, detail)
}

/**
 * Whether a mode-read error is transient and worth a brief automatic retry. A refusal with
 * `retryable: false` (such as `503 cell.unopenable`) and non-transient 4xx answers are never
 * retried automatically, so Studio settles immediately into an honest unavailable state.
 */
export function isRetryableModeError(error: unknown): boolean {
  if (!(error instanceof CellUnavailable)) return false
  if (error.retryable === false) return false
  if (error.status >= 400 && error.status < 500 && error.status !== 408 && error.status !== 429) return false
  return true
}

/**
 * Brief, bounded automatic retries for transient outages (~5.6 s total) before settling into a
 * persistent Workspace unavailable state. Non-retryable refusals (`retryable: false`) settle
 * immediately without ever rendering "Retrying…".
 */
export const MODE_AUTO_RETRIES = 3

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
 * Synchronize Studio's client caches when a scoped read or write discovers `/api/me` moved to a new
 * scope (org or environment): invalidates the old mode/catalog, cancels and drops old-scope queries,
 * and closes capability gates immediately. When `nextScope` is `undefined` (for example, a write
 * received `409 workspace.changed` but the follow-up `/api/me` read failed during an outage), stale
 * caches are purged and `["studio-scope"]` is invalidated without writing `null` into the cache so
 * Studio never falls back to recorded mode during an outage.
 */
export function syncStudioScopeChange(client: QueryClient, nextScope: StudioScope | null | undefined): void {
  if (nextScope === undefined) {
    lastObservedScope.delete(client)
    refreshMode()
    purgeStaleScopeQueries(client, "")
    void client.resetQueries({ queryKey: ["studio-scope"], exact: true })
    void client.invalidateQueries({ queryKey: ["session"] })
    return
  }
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
    if (scope.isSuccess) scopeAnswered(client, activeScope)
  }, [client, scope.isSuccess, key, activeScope])

  return useQuery(modeQuery(scopeOutage ? undefined : activeScope, scopeReady, scopeOutage ? client : undefined))
}

/**
 * What {@link useMode} does each time the page's scope query answers `scope` (`["studio-scope"]` succeeds).
 *
 * The answer ends outage recovery for the page: from now on the page shows `scope`'s own mode query, which
 * may already hold that workspace's mode and then reads nothing. So a failure the outage query settled while
 * no workspace was active (a session outage, or a refusal from the workspace recovery discovered) is dropped
 * here, whichever workspace the scope names. A failure settled for the active workspace is kept: it is that
 * workspace's own, and only Retry or a scope switch clears it.
 */
export function scopeAnswered(client: QueryClient, scope: StudioScope | null): void {
  if (activeModeScopeKey === null) settledModeFailure = undefined
  const key = studioScopeKey(scope)
  const prev = lastObservedScope.get(client)
  lastObservedScope.set(client, key)
  if (prev !== undefined && prev !== key) {
    if (activeModeScopeKey !== key) refreshMode()
    purgeStaleScopeQueries(client, key)
    maybeInvalidateSession(client, scope)
  }
}

export function modeQuery(scope: StudioScope | null | undefined, enabled = true, recoveryClient?: QueryClient) {
  return queryOptions({
    queryKey: ["studio-mode", scope === undefined ? "outage" : studioScopeKey(scope)],
    enabled,
    queryFn: async ({ signal }) => {
      const mode = await (recoveryClient === undefined ? currentMode(browserFetch, scope, signal) : readMode(browserFetch, undefined, signal))
      // A mode read succeeded: forget the failure settled in this read's own generation for the workspace
      // it read, or a settled session outage (the session server answered this read). A success from an
      // older generation (a recovery read that outlived `refreshMode`) never clears a newer settle.
      const settled = settledModeFailure
      const target = definitionsTargetOf(mode)
      if (
        settled !== undefined
        && target !== undefined
        && target.generation === settled.generation
        && (settled.scopeKey === null || settled.scopeKey === target.scopeKey)
      ) {
        settledModeFailure = undefined
      }
      if (recoveryClient !== undefined) recoveryClient.setQueryData(["studio-scope"], mode.kind === "live" ? mode.scope : null)
      return mode
    },
    retry: (failureCount, error) => {
      if (error instanceof ScopeSupersededError) return false
      if (isRetryableModeError(error) && failureCount < MODE_AUTO_RETRIES) return true
      // Settled: no more automatic retries here, and none through the transport either. Bind the error
      // to where its read went, and record it only in the current generation and while that workspace
      // is the active one or none is: outage recovery can discover (and be refused by) another
      // workspace than the active one, and that refusal is never the active workspace's failure.
      const target = definitionsTargetOf(error)
      if (
        target !== undefined
        && target.generation === modeGeneration
        && (activeModeScopeKey === null || target.scopeKey === activeModeScopeKey)
      ) {
        settledModeFailure = { error, ...target }
      }
      return false
    },
    retryDelay: (attempt) => Math.min(800 * 2 ** attempt, 3200),
    staleTime: Infinity,
  })
}

/** Stop in-flight scope and mode probes (including reads the transport started directly) and start fresh replacements. */
export async function retryMode(client: QueryClient): Promise<void> {
  const scopeFilter = { queryKey: ["studio-scope"], type: "active" } as const
  const modeFilter = { queryKey: ["studio-mode"], type: "active" } as const
  const scopeState = client.getQueryState(["studio-scope"])
  const retryScope =
    scopeState !== undefined
    && (scopeState.fetchStatus === "fetching" || scopeState.status === "error" || scopeState.status === "pending")
  const cancellations = [
    client.cancelQueries(modeFilter),
    ...(retryScope ? [client.cancelQueries(scopeFilter)] : []),
  ]
  for (const controller of pendingModeControllers) controller.abort()
  refreshMode()
  await Promise.all(cancellations)
  if (retryScope) {
    await client.refetchQueries(scopeFilter, { cancelRefetch: false })
    const nextScope = client.getQueryState<StudioScope | null>(["studio-scope"])
    const nextModeKey =
      nextScope?.status === "success"
        ? studioScopeKey(nextScope.data)
        : nextScope?.status === "error"
          ? "outage"
          : null
    if (nextModeKey !== null) {
      await client.refetchQueries({ queryKey: ["studio-mode", nextModeKey], exact: true, type: "active" }, { cancelRefetch: false })
    }
    return
  }
  await client.refetchQueries(modeFilter, { cancelRefetch: false })
}

/** Forget the decided mode, so the next read takes the cell's current catalog (after a load installs a new type or scope changes). */
export function refreshMode(): void {
  modeGeneration += 1
  activeModeScopeKey = null
  modeByScope.clear()
  unkeyedMode = undefined
  settledModeFailure = undefined
}
