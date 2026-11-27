/**
 * Studio over a connected cell: which documents the pages read (the cell's, or
 * the recording when no cell is connected) and the live reads themselves. Kept
 * apart from transport.ts, which finds recordings with Vite's import.meta.glob.
 */
import { onlineManager, queryOptions, type QueryClient, useQuery, useQueryClient } from "@tanstack/react-query"
import { useEffect } from "react"
import { CellUnavailable, StudioScopeError, defaultFetch, isReadMethod, parseUnavailableDetail, readStudioScope, recordCellReachable, resolveWriteScope, sameStudioScope, scopedHostFetch, studioScopeKey, useStudioScope, type ScopedRequestInit, type StudioScope, type UnavailableDetail } from "./definitions.ts"
import { NO_HOST_ERROR } from "../../../../packages/norn-sdk/src/index.ts"
import { readDocument, type Read } from "./exact.ts"
import type { Artifact, Envelope, Transport } from "./transport.ts"
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
    /** The built-in pages the cell serves beside its definitions (`builtins`): see {@link builtinsOf}. */
    readonly builtins: readonly Envelope[]
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
 * The catalog's `builtins`: Studio's pages the cell serves built in, each only while the workspace
 * installed no definition with its id (norn-api `builtin_pages`). The cell decides which it serves;
 * Studio holds no page of its own.
 */
export function builtinsOf(document: unknown): readonly Envelope[] {
  const raw = (document as { builtins?: unknown } | null)?.builtins
  if (!Array.isArray(raw)) return []
  return raw.flatMap((builtin: { artifact?: unknown } | null) => {
    const artifact = builtin?.artifact
    return artifact !== null && typeof artifact === "object" && !Array.isArray(artifact) ? [artifact as Envelope] : []
  })
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
    builtins: builtinsOf(read.document),
  }
}

/**
 * The live transport over one connected cell: the catalog and counts its mode read carried. It reads
 * nothing itself; records are read through the decided workspace's reader (`lib/workspace.tsx`).
 */
export function liveTransport(catalog: Read<readonly Artifact[]>, counts: ObjectCounts = {}, builtins: readonly Envelope[] = []): Transport {
  return {
    catalog: async () => catalog,
    builtins: async () => builtins,
    objectCounts: async () => counts,
    definitions: async () => catalog.document.map((artifact, index) => subRead(catalog, `/${index}`, artifact as Envelope)),
    questionBoard: async () => { throw new NotServed("Saved questions are") },
    restatement: async () => { throw new NotServed("Restated answers are") },
    guarantees: async () => { throw new NotServed("Model-checked proofs are") },
  }
}

/** Raised when an in-flight mode read settles after a newer scope or `refreshMode()` superseded it. */
export class ScopeSupersededError extends Error {
  constructor() { super("The active workspace scope changed while reading cell capabilities.") }
}

let modeGeneration = 0
let activeModeScopeKey: string | null = null
type ModeProbe = { readonly controller: AbortController; readonly result: Promise<Mode>; observers: number; settled: boolean }
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
  const probe: ModeProbe = {
    controller,
    observers: 0,
    settled: false,
    result: read(controller.signal).finally(() => {
      probe.settled = true
      pendingModeControllers.delete(controller)
    }),
  }
  return probe
}

function abortErrorOf(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException("The operation was aborted.", "AbortError")
}

function observeModeProbe(probe: ModeProbe, signal?: AbortSignal): Promise<Mode> {
  if (signal === undefined) {
    probe.observers += 1
    return probe.result
  }
  if (signal.aborted) {
    return Promise.reject(abortErrorOf(signal))
  }
  probe.observers += 1
  return new Promise<Mode>((resolve, reject) => {
    let done = false
    const release = () => {
      if (done) return
      done = true
      signal.removeEventListener("abort", onAbort)
      probe.observers = Math.max(0, probe.observers - 1)
    }
    const onAbort = () => {
      release()
      if (!probe.settled && probe.observers === 0) probe.controller.abort(signal.reason)
      reject(abortErrorOf(signal))
    }
    signal.addEventListener("abort", onAbort, { once: true })
    probe.result.then(
      (mode) => {
        if (done) return
        release()
        resolve(mode)
      },
      (error: unknown) => {
        if (done) return
        release()
        reject(error)
      },
    )
  })
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
  if (signal?.aborted) return Promise.reject(abortErrorOf(signal))
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
  retrySerial += 1
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

const completedModeGenerationByScope = new Map<string, number>()
let retrySerial = 0
const retryWaiters = new Set<() => void>()
const notifyRetryWaiters = () => {
  for (const wake of retryWaiters) wake()
}

async function awaitRetryStep<T>(
  myRetry: number,
  run: () => Promise<T>,
  onAbort: () => void,
): Promise<{ readonly aborted: true } | { readonly aborted: false; readonly value: T }> {
  if (retrySerial !== myRetry || !onlineManager.isOnline()) {
    onAbort()
    return { aborted: true }
  }
  let settled = false
  let wakeAbort!: () => void
  const aborted = new Promise<{ readonly aborted: true }>((resolve) => {
    wakeAbort = () => {
      if (settled) return
      settled = true
      onAbort()
      resolve({ aborted: true })
    }
  })
  const onRetryBump = () => {
    if (retrySerial !== myRetry) wakeAbort()
  }
  retryWaiters.add(onRetryBump)
  const unsubscribeOnline = onlineManager.subscribe((online) => {
    if (!online) wakeAbort()
  })
  try {
    const result = await Promise.race([
      run().then(
        (value) => ({ aborted: false as const, value }),
        (error) => {
          if (settled) return { aborted: true as const }
          throw error
        },
      ),
      aborted,
    ])
    settled = true
    return result
  } finally {
    settled = true
    retryWaiters.delete(onRetryBump)
    unsubscribeOnline()
  }
}

export function modeQuery(scope: StudioScope | null | undefined, enabled = true, recoveryClient?: QueryClient) {
  const modeScopeKey = scope === undefined ? "outage" : studioScopeKey(scope)
  return queryOptions({
    queryKey: ["studio-mode", modeScopeKey],
    enabled,
    queryFn: async ({ signal }) => {
      const generation = modeGeneration
      const mode = await (recoveryClient === undefined ? currentMode(browserFetch, scope, signal) : readMode(browserFetch, undefined, signal))
      // Record by scope key (not by `mode` object identity) so TanStack Query structural sharing
      // keeping a prior object reference never loses the completed generation marker.
      const prevGen = completedModeGenerationByScope.get(modeScopeKey) ?? 0
      if (generation >= prevGen) completedModeGenerationByScope.set(modeScopeKey, generation)
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

async function retryModeWithMeta(client: QueryClient): Promise<{ readonly ok: boolean; readonly serial: number; readonly generation: number }> {
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
  retrySerial += 1
  const myRetry = retrySerial
  notifyRetryWaiters()
  refreshMode()
  const generation = modeGeneration
  const abortProbes = () => {
    for (const controller of pendingModeControllers) controller.abort()
    void client.cancelQueries(modeFilter)
    if (retryScope) void client.cancelQueries(scopeFilter)
  }
  await Promise.all(cancellations)
  if (retrySerial !== myRetry || !onlineManager.isOnline()) return { ok: false, serial: myRetry, generation }
  if (retryScope) {
    const scopeStep = await awaitRetryStep(
      myRetry,
      () => client.refetchQueries(scopeFilter, { cancelRefetch: false }),
      abortProbes,
    )
    if (scopeStep.aborted || retrySerial !== myRetry || !onlineManager.isOnline()) {
      return { ok: false, serial: myRetry, generation }
    }
    const nextScope = client.getQueryState<StudioScope | null>(["studio-scope"])
    if (nextScope?.status === "success" && nextScope.fetchStatus === "idle") {
      const scope = nextScope.data ?? null
      scopeAnswered(client, scope)
      // `scopeAnswered` may synchronously advance `modeGeneration` when the workspace changed;
      // bind the mode fetch check to the generation active right before `fetchQuery`.
      const fetchGeneration = modeGeneration
      const scopeKey = studioScopeKey(scope)
      // Read always (`staleTime: 0`): Retry decides the mode again even when this workspace's is cached.
      // A mode query of this key that `useMode` mounts meanwhile joins this read.
      const fetchStep = await awaitRetryStep(
        myRetry,
        () => client.fetchQuery({ ...modeQuery(scope), staleTime: 0 }).then(() => true, () => false),
        abortProbes,
      )
      const ok =
        !fetchStep.aborted
        && fetchStep.value
        && completedModeGenerationByScope.get(scopeKey) === fetchGeneration
        && retrySerial === myRetry
        && modeGeneration === fetchGeneration
        && onlineManager.isOnline()
      return { ok, serial: myRetry, generation: fetchGeneration }
    }
    if (nextScope?.status !== "error" || nextScope.fetchStatus !== "idle") {
      return { ok: false, serial: myRetry, generation }
    }
    const outage = ["studio-mode", "outage"]
    const outageStep = await awaitRetryStep(
      myRetry,
      () => client.refetchQueries({ queryKey: outage, exact: true, type: "active" }, { cancelRefetch: false }),
      abortProbes,
    )
    if (outageStep.aborted || retrySerial !== myRetry || modeGeneration !== generation || !onlineManager.isOnline()) {
      return { ok: false, serial: myRetry, generation }
    }
    const outageState = client.getQueryState<Mode>(outage)
    const ok =
      outageState?.status === "success"
      && outageState.fetchStatus === "idle"
      && outageState.data !== undefined
      && completedModeGenerationByScope.get("outage") === generation
    return { ok, serial: myRetry, generation }
  }
  const modeStep = await awaitRetryStep(
    myRetry,
    () => client.refetchQueries(modeFilter, { cancelRefetch: false }),
    abortProbes,
  )
  if (modeStep.aborted || retrySerial !== myRetry || modeGeneration !== generation || !onlineManager.isOnline()) {
    return { ok: false, serial: myRetry, generation }
  }
  const active = client.getQueryCache().findAll(modeFilter)
  const ok = active.every((query) =>
    query.state.status === "success"
    && query.state.fetchStatus === "idle"
    && typeof query.state.data === "object"
    && query.state.data !== null
    && completedModeGenerationByScope.get(String(query.queryKey[1])) === generation,
  )
  return { ok, serial: myRetry, generation }
}

/**
 * Stop in-flight scope and mode probes (including reads the transport started directly) and start fresh
 * replacements. Resolves `true` once the page's mode is decided again in this retry's generation, `false`
 * when it settled in error, was superseded by a newer retry/scope switch, or could not run (offline/unmounted).
 *
 * When the scope is read again and answers, that answer is applied here, at once ({@link scopeAnswered}:
 * a different workspace purges the old one's reads), and that workspace's mode is read here too, not
 * left to `useMode`'s next render: until it runs, no mode query of the new key is mounted, and whatever
 * reads again next would discover the scope a second time or read in the old workspace.
 */
export async function retryMode(client: QueryClient): Promise<boolean> {
  return (await retryModeWithMeta(client)).ok
}

/**
 * Whether a read's answer is a refusal held as data, not as the query's error: a pushed refusal
 * (`CellRefused`, `{ kind: "refused" }`) or a cell call's refused result (`{ result: { kind: "refused" } }`,
 * such as the decisions inbox). The query succeeded, but what it shows is the refusal, and its page may
 * show it as Workspace unavailable, whose Retry is {@link retryWorkspace}.
 */
function holdsRefusal(data: unknown): boolean {
  const refused = (value: unknown) => typeof value === "object" && value !== null && (value as { readonly kind?: unknown }).kind === "refused"
  return refused(data) || (typeof data === "object" && data !== null && refused((data as { readonly result?: unknown }).result))
}

/**
 * The one user Retry for the workspace (the shell's one-line status, a page's own Workspace unavailable
 * state, a skeleton's Retry while the mode is read): decide the mode again (`retryMode`), and only once
 * it is decided read again every read on screen that errored or holds a refusal. Sequenced, so a page's
 * read never runs while the mode is still undecided (that read would discover the scope a second time);
 * a read that is already running again is joined, not restarted. When the mode settles in error nothing
 * else is read: the page shows Workspace unavailable for the mode, and its reads would only ask the cell
 * that did not answer again (each with its own automatic retries, keeping Retry disabled meanwhile).
 * Resolves when both are done, so a Retry button can stay disabled until then.
 */
export async function retryWorkspace(client: QueryClient): Promise<boolean> {
  const result = await retryModeWithMeta(client)
  if (!result.ok || retrySerial !== result.serial || modeGeneration !== result.generation || !onlineManager.isOnline()) {
    return false
  }
  const invalidateStep = await awaitRetryStep(
    result.serial,
    () => client.invalidateQueries({
      predicate: (query) => {
        const head = query.queryKey[0]
        if (head === "studio-scope" || head === "studio-mode") return false
        return query.state.status === "error" || holdsRefusal(query.state.data)
      },
    }, { cancelRefetch: false }),
    () => {},
  )
  return !invalidateStep.aborted
    && retrySerial === result.serial
    && modeGeneration === result.generation
    && onlineManager.isOnline()
}

/** Forget the decided mode, so the next read takes the cell's current catalog (after a load installs a new type or scope changes). */
export function refreshMode(): void {
  modeGeneration += 1
  activeModeScopeKey = null
  modeByScope.clear()
  unkeyedMode = undefined
  settledModeFailure = undefined
}
