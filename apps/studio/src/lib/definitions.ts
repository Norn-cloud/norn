import { createContext, use } from "react"
import { QueryClientContext, queryOptions, useQuery } from "@tanstack/react-query"

/**
 * Dashboards are definitions in the cell: an ordered list of admitted
 * QuestionIRs with a title and a layout. The cell admits a definition (its
 * checker) and installs it (a new version); Studio only shows what it says.
 * CELL-DO's contract (2026-11-06): `cellDefinitions` below. Where the cell
 * does not serve the route yet, every call says nothing was saved.
 */
export interface Tile {
  readonly id: string
  /** The reader's title for the question; the QuestionIR's name is derived from it. */
  readonly title: string
  readonly question: Record<string, unknown> & { readonly name: string }
  /** How the tile draws its answer; the cell admits a number only for one value, a pie only for the parts of one whole. */
  readonly view: { readonly kind: "chart" | "table" | "number"; readonly chart?: "line" | "bar" | "area" | "pie" }
  readonly layout: { readonly x: number; readonly y: number; readonly w: number; readonly h: number }
}
/** Who authored a dashboard version; an assistant's proposal names itself, its model and the request it answered. */
export interface Origin {
  readonly by: "studio" | "code" | "assistant"
  readonly proposal?: string
  readonly model?: string
  readonly request?: string
}
export interface Dashboard {
  readonly title: string
  readonly tiles: readonly Tile[]
  readonly origin?: Origin
}
export interface StoredDashboard {
  readonly id: string
  readonly version: number
  readonly dashboard: Dashboard
}
/** One row of the definitions list: no payload, so open it to read the dashboard. */
export interface DashboardSummary {
  readonly id: string
  readonly title: string
  readonly version: number
  readonly lifecycle: string
  readonly updatedAtMs: number
}
export interface DashboardPage { readonly items: readonly DashboardSummary[]; readonly cursor: string | null }
/** One cell finding: its code, the law that refused (when one did), and a JSON pointer into the payload. */
export interface Finding { readonly code: string; readonly message: string; readonly pointer: string | null; readonly law?: string | null }
export type Admission = { readonly admitted: true } | { readonly admitted: false; readonly findings: readonly Finding[] }
export interface Receipt { readonly receiptHash: string; readonly commitSeq: number; readonly recordedAtMs: number }
export type Installed =
  | { readonly installed: true; readonly id: string; readonly version: number; readonly receipt: Receipt }
  | { readonly installed: false; readonly findings: readonly Finding[] }
  /** Someone installed a newer version since this one was opened. */
  | { readonly installed: false; readonly conflict: true }

export interface DefinitionStore {
  /** One page of saved dashboards (retired ones left out); pass the page's cursor for the next. */
  list(cursor?: string | null): Promise<DashboardPage>
  /** The dashboard's current version and payload; `undefined` when no such definition exists. */
  get(id: string): Promise<StoredDashboard | undefined>
  /** Side-effect free: the cell's checker on this payload, for live validation. */
  admit(id: string, dashboard: Dashboard): Promise<Admission>
  /** A new version of `id`; refused as a conflict if `expectedVersion` is no longer current. Without it, a new definition (or an explicit overwrite). */
  install(id: string, dashboard: Dashboard, expectedVersion?: number): Promise<Installed>
  /** Retires the definition: it leaves the list, and its history stays in the cell. */
  retire(id: string, expectedVersion: number): Promise<Installed>
}

/**
 * A new dashboard's definition id: its title when first saved, plus a random
 * suffix, so two dashboards whose titles read alike never share an id. An
 * install without `expectedVersion` overwrites, and the cell documents no
 * create-only install, so distinct ids are what keep a new dashboard from
 * replacing another. Reopened dashboards keep the id they were saved under.
 */
export const dashboardId = (title: string, unique: string = crypto.randomUUID().replaceAll("-", "").slice(0, 12)) =>
  `studio.dashboard.${title.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "untitled"}_${unique}`

const isObject = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object"
/** A tile the page can draw: TileCard reads its question name, view kind and layout width. */
const isTile = (value: unknown): value is Tile =>
  isObject(value) && typeof value.id === "string" && typeof value.title === "string"
  && isObject(value.question) && typeof value.question.name === "string"
  && isObject(value.view) && (value.view.kind === "chart" || value.view.kind === "table" || value.view.kind === "number")
  && isObject(value.layout) && typeof value.layout.w === "number"
const isDashboard = (value: unknown): value is Dashboard =>
  isObject(value) && typeof value.title === "string" && Array.isArray(value.tiles) && value.tiles.every(isTile)

const KIND = "dashboard"
type Fetch = (input: string, init?: RequestInit) => Promise<Response>
/** Canonical browser fetch wrapper shared across Studio clients so `scopedHostFetch` recognizes it. */
export const defaultFetch: Fetch = (input, init) => fetch(input, init)
export const isDefaultFetch = (fetcher: Fetch): boolean => fetcher === fetch || fetcher === defaultFetch
export interface WorkspaceScope { readonly orgId: string; readonly environmentId: string | null }
export interface StudioScope { readonly orgId: string; readonly environmentId: string }
export type ScopeProvider = (signal?: AbortSignal) => Promise<StudioScope | null>
export const NO_DATA_CELL = "This workspace has no data cell yet."
export const EXPECTED_ORG_HEADER = "x-norn-expected-org"
export const EXPECTED_ENV_HEADER = "x-norn-expected-env"
export class StudioScopeError extends Error {}

function parseAccountScope(body: unknown): { readonly organization: { readonly id: string } | null; readonly environmentId: string | null } | null {
  if (!isObject(body) || Array.isArray(body)) return null
  const org = body.organization
  let organization: { readonly id: string } | null
  if (org === null) {
    organization = null
  } else if (isObject(org) && !Array.isArray(org) && typeof org.id === "string" && org.id.length > 0) {
    organization = { id: org.id }
  } else {
    return null
  }
  const env = body.environmentId
  let environmentId: string | null
  if (env === null) {
    environmentId = null
  } else if (typeof env === "string" && env.length > 0) {
    environmentId = env
  } else {
    return null
  }
  return { organization, environmentId }
}

/** Structured refusal metadata preserved when a cell answers with `norn/refusal@2` or times out. */
export interface UnavailableDetail {
  readonly code?: string
  readonly retryable?: boolean
  readonly explanation?: string
  readonly incidentId?: string
  readonly category?: string
  readonly timedOut?: boolean
  readonly decidedAt?: string
}

/** Parse structured fields from a `norn/refusal@2` (or Worker error) JSON object. */
export function parseUnavailableDetail(body: unknown): UnavailableDetail {
  if (body === null || typeof body !== "object" || Array.isArray(body)) return {}
  const record = body as Record<string, unknown>
  const explanation = typeof record.explanation === "string" && record.explanation !== ""
    ? record.explanation
    : typeof record.error === "string" && record.error !== "" ? record.error : undefined
  return {
    ...(typeof record.code === "string" ? { code: record.code } : {}),
    ...(typeof record.retryable === "boolean" ? { retryable: record.retryable } : {}),
    ...(explanation !== undefined ? { explanation } : {}),
    ...(typeof record.incidentId === "string" && record.incidentId !== "" ? { incidentId: record.incidentId } : {}),
    ...(typeof record.category === "string" ? { category: record.category } : {}),
    ...(typeof record.decidedAt === "string" && record.decidedAt !== "" ? { decidedAt: record.decidedAt } : {}),
  }
}

const REACHABLE_KEY = "norn.cell.lastReachableAt"
const reachableByScope = new Map<string, number>()

const reachableStorageKey = (scope: StudioScope): string => `${REACHABLE_KEY}:${studioScopeKey(scope)}`

/** Record that `scope`'s workspace cell answered cleanly at `at` (Unix ms). */
export function recordCellReachable(scope: StudioScope | null | undefined, at = Date.now()): void {
  if (scope == null) return
  const key = studioScopeKey(scope)
  reachableByScope.set(key, at)
  try { globalThis.sessionStorage?.setItem(reachableStorageKey(scope), String(at)) } catch { /* storage blocked or SSR */ }
}

/** The last instant (Unix ms) `scope`'s workspace cell answered cleanly in this session, if any. */
export function lastReachableAt(scope: StudioScope | null | undefined): number | null {
  if (scope == null) return null
  const key = studioScopeKey(scope)
  const cached = reachableByScope.get(key)
  if (cached !== undefined) return cached
  try {
    const stored = globalThis.sessionStorage?.getItem(reachableStorageKey(scope))
    if (stored) {
      const parsed = Number(stored)
      if (Number.isFinite(parsed) && parsed > 0) { reachableByScope.set(key, parsed); return parsed }
    }
  } catch { /* storage blocked or SSR */ }
  return null
}

/** Reset remembered reachability across all scopes (for tests). */
export function resetCellReachable(): void {
  reachableByScope.clear()
  try {
    const storage = globalThis.sessionStorage
    if (!storage) return
    const toRemove: string[] = []
    for (let i = 0; i < storage.length; i += 1) {
      const key = storage.key(i)
      if (key !== null && (key === REACHABLE_KEY || key.startsWith(`${REACHABLE_KEY}:`))) {
        toRemove.push(key)
      }
    }
    for (const key of toRemove) storage.removeItem(key)
  } catch { /* ignore */ }
}

/** Default per-request deadline (ms) for idempotent reads so a hung host never spins a surface forever. */
export const CELL_FETCH_TIMEOUT_MS = 12_000

const READ_METHODS = new Set(["GET", "HEAD", "OPTIONS"])
const NULL_BODY_STATUSES = new Set([101, 204, 205, 304])

/** Whether an HTTP method is an idempotent read eligible for the default read deadline. */
export function isReadMethod(method?: string): boolean {
  return method === undefined || READ_METHODS.has(method.toUpperCase())
}

/** RequestInit with an optional per-call deadline override (reads only; writes supply their own deadline). */
export type ScopedRequestInit = RequestInit & { readonly timeoutMs?: number }

const timeoutError = (): CellUnavailable =>
  new CellUnavailable(0, "The workspace did not answer in time.", { timedOut: true, retryable: true, code: "cell.timeout" })

/**
 * Bound one fetch call with a deadline that stays active through response body consumption,
 * preserving any caller-supplied AbortSignal. Unexpected transport/stream errors normalize into a
 * retryable `CellUnavailable(0)` unless the caller aborted the request.
 */
export async function fetchWithTimeout(
  fetcher: Fetch,
  input: string,
  init?: RequestInit,
  timeoutMs = CELL_FETCH_TIMEOUT_MS,
  onComplete?: () => void,
): Promise<Response> {
  const callerSignal = init?.signal
  if (callerSignal?.aborted) {
    throw callerSignal.reason ?? new DOMException("Aborted", "AbortError")
  }
  const controller = new AbortController()
  let timedOut = false
  let cleaned = false
  const onCallerAbort = () => controller.abort(callerSignal?.reason ?? new DOMException("Aborted", "AbortError"))
  const timer = setTimeout(() => {
    timedOut = true
    controller.abort(new DOMException("Timed out", "TimeoutError"))
  }, timeoutMs)
  const cleanup = () => {
    if (cleaned) return
    cleaned = true
    clearTimeout(timer)
    callerSignal?.removeEventListener("abort", onCallerAbort)
  }
  callerSignal?.addEventListener("abort", onCallerAbort, { once: true })

  let response: Response
  try {
    response = await fetcher(input, { ...init, signal: controller.signal })
  } catch (error) {
    cleanup()
    if (callerSignal?.aborted) throw error
    const isTimeout = timedOut
      || controller.signal.aborted
      || (error instanceof DOMException && (error.name === "TimeoutError" || error.name === "AbortError"))
      || (error instanceof Error && (error.name === "TimeoutError" || error.name === "AbortError"))
    if (isTimeout) throw timeoutError()
    if (error instanceof CellUnavailable || error instanceof StudioScopeError) throw error
    throw new CellUnavailable(0, undefined, { retryable: true })
  }

  if (response.body === null || NULL_BODY_STATUSES.has(response.status)) {
    cleanup()
    if (response.ok) onComplete?.()
    return response
  }

  const reader = response.body.getReader()
  const abortReason = (): unknown =>
    callerSignal?.aborted
      ? (callerSignal.reason ?? new DOMException("Aborted", "AbortError"))
      : timeoutError()

  let onAbort: (() => void) | null = null
  let streamFailure: unknown = undefined
  const finish = () => {
    if (onAbort !== null) {
      controller.signal.removeEventListener("abort", onAbort)
      onAbort = null
    }
    cleanup()
  }

  const stream = new ReadableStream<Uint8Array>({
    start(streamController) {
      onAbort = () => {
        const reason = abortReason()
        streamFailure = reason
        finish()
        void reader.cancel(reason).catch(() => {})
        try { streamController.error(reason) } catch { /* already closed or errored */ }
      }
      if (controller.signal.aborted) {
        onAbort()
        return
      }
      controller.signal.addEventListener("abort", onAbort, { once: true })
    },
    async pull(streamController) {
      if (controller.signal.aborted) {
        const reason = abortReason()
        streamFailure = reason
        finish()
        void reader.cancel(reason).catch(() => {})
        try { streamController.error(reason) } catch { /* already closed or errored */ }
        return
      }
      try {
        const { done, value } = await reader.read()
        if (controller.signal.aborted) {
          const reason = abortReason()
          streamFailure = reason
          finish()
          void reader.cancel(reason).catch(() => {})
          try { streamController.error(reason) } catch { /* already closed or errored */ }
          return
        }
        if (done) {
          finish()
          if (response.ok) onComplete?.()
          try { streamController.close() } catch { /* already closed */ }
          return
        }
        streamController.enqueue(value)
      } catch (error) {
        finish()
        const normalized = callerSignal?.aborted
          ? error
          : (timedOut || controller.signal.aborted
            ? timeoutError()
            : (error instanceof CellUnavailable || error instanceof StudioScopeError
              ? error
              : new CellUnavailable(0, undefined, { retryable: true })))
        streamFailure = normalized
        try { streamController.error(normalized) } catch { /* already errored */ }
      }
    },
    async cancel(reason) {
      finish()
      await reader.cancel(reason).catch(() => {})
    },
  })

  const wrapped = new Response(stream, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  })
  // Chromium's BodyStreamBuffer may rewrite custom ReadableStream errors into generic TypeErrors
  // during .text()/.json()/.arrayBuffer(); rethrow the exact bound timeout/abort failure instead.
  const preserveFailure = (error: unknown): never => {
    if (streamFailure !== undefined) throw streamFailure
    throw error
  }
  const rawText = wrapped.text.bind(wrapped)
  wrapped.text = () => rawText().catch(preserveFailure)
  const rawJson = wrapped.json.bind(wrapped)
  wrapped.json = () => rawJson().catch(preserveFailure)
  const rawArrayBuffer = wrapped.arrayBuffer.bind(wrapped)
  wrapped.arrayBuffer = () => rawArrayBuffer().catch(preserveFailure)
  return wrapped
}

/**
 * The workspace scope currently displayed by this tab, populated whenever `accountQuery` loads or
 * refreshes `/api/me`. Undefined until a browser session loads (so isolated unit tests passing
 * custom fetchers keep calling `readStudioScope(fetcher)`). Writes stay bound to the scope that
 * loaded the form/workflow (or this displayed scope) and send it explicitly so a workspace switch
 * in another tab cannot redirect a stale form.
 */
let currentDisplayedScope: StudioScope | null | undefined
let currentDisplayedWorkspaceScope: WorkspaceScope | null | undefined

export function getDisplayedScope(): StudioScope | null | undefined {
  return currentDisplayedScope
}

export function getDisplayedWorkspaceScope(): WorkspaceScope | null | undefined {
  return currentDisplayedWorkspaceScope
}

export function setDisplayedScope(scope: StudioScope | null | undefined): void {
  currentDisplayedScope = scope
  currentDisplayedWorkspaceScope = scope
}

export function setDisplayedWorkspaceScope(scope: WorkspaceScope | null | undefined): void {
  currentDisplayedWorkspaceScope = scope
  currentDisplayedScope = scope === undefined
    ? undefined
    : scope === null || scope.environmentId === null
      ? null
      : { orgId: scope.orgId, environmentId: scope.environmentId }
}

export function workspaceScopeFromAccount(account: {
  readonly organization?: { readonly id: string } | null
  readonly environmentId?: string | null
} | null | undefined): WorkspaceScope | null {
  const orgId = account?.organization?.id
  if (typeof orgId !== "string" || orgId === "") return null
  const environmentId = typeof account?.environmentId === "string" && account.environmentId !== "" ? account.environmentId : null
  return { orgId, environmentId }
}

export function scopeFromAccount(account: {
  readonly organization?: { readonly id: string } | null
  readonly environmentId?: string | null
} | null | undefined): StudioScope | null {
  const ws = workspaceScopeFromAccount(account)
  return ws !== null && ws.environmentId !== null ? { orgId: ws.orgId, environmentId: ws.environmentId } : null
}

/** Expected-scope headers sent on cookie-authenticated mutations. */
export function scopeHeaders(scope: WorkspaceScope | StudioScope | null | undefined): Record<string, string> {
  if (scope === null || scope === undefined) return {}
  return {
    [EXPECTED_ORG_HEADER]: scope.orgId,
    ...scope.environmentId !== null && scope.environmentId !== "" ? { [EXPECTED_ENV_HEADER]: scope.environmentId } : {},
  }
}

/** Stable cache key for one server-reported `(orgId, environmentId)` scope. */
export function studioScopeKey(scope: StudioScope | null | undefined): string {
  return scope == null ? "none" : JSON.stringify([scope.orgId, scope.environmentId])
}

/** Whether two server-reported scopes identify the exact same org and cell environment. */
export function sameStudioScope(a: StudioScope | null | undefined, b: StudioScope | null | undefined): boolean {
  if (a == null || b == null) return a == null && b == null
  return a.orgId === b.orgId && a.environmentId === b.environmentId
}

/** Shown when a scope-bound read or write is refused because the active workspace diverged. */
export const WORKSPACE_CHANGED_MESSAGE = "This workspace changed; reload to edit this view."

/**
 * The workspace failure a Widget UX read's answer carries as its cause (`readWidgetAtScope` answers
 * `network_failure` with `{ workspace }`): the session's workspace, not the cell, did not answer.
 */
export function unavailableWorkspaceOf(cause: unknown): CellUnavailable | undefined {
  const workspace = typeof cause === "object" && cause !== null ? (cause as { readonly workspace?: unknown }).workspace : undefined
  return workspace instanceof CellUnavailable ? workspace : undefined
}

/**
 * Run a scope-pinned read only while `readScope()` matches `expectedScope` before dispatch and
 * after resolution, discarding any late result if the workspace changed in flight.
 */
export async function readAtScope<T>(
  expectedScope: StudioScope,
  readScope: ScopeProvider,
  read: () => Promise<T>,
  signal?: AbortSignal,
  mismatchMessage = WORKSPACE_CHANGED_MESSAGE,
): Promise<T> {
  if (signal?.aborted) throw new StudioScopeError(mismatchMessage)
  const before = await readScope(signal)
  if (signal?.aborted || !sameStudioScope(before, expectedScope)) {
    throw new StudioScopeError(mismatchMessage)
  }
  const result = await read()
  if (signal?.aborted) throw new StudioScopeError(mismatchMessage)
  const after = await readScope(signal)
  if (signal?.aborted || !sameStudioScope(after, expectedScope)) {
    throw new StudioScopeError(mismatchMessage)
  }
  return result
}

/** Read the active workspace and its cell from the signed-in server session. */
export async function readStudioScope(fetcher: Fetch = defaultFetch, signal?: AbortSignal): Promise<StudioScope | null> {
  signal?.throwIfAborted()
  let response: Response
  let text: string
  try {
    response = await fetchWithTimeout(fetcher, "/api/me", { headers: { accept: "application/json" }, ...(signal === undefined ? {} : { signal }) })
    text = await response.text()
  } catch (error) {
    signal?.throwIfAborted()
    if (error instanceof Error && error.name === "AbortError") throw error
    if (error instanceof CellUnavailable) throw error
    throw new CellUnavailable(0, undefined, { retryable: true })
  }
  signal?.throwIfAborted()
  let body: unknown = null
  try { body = JSON.parse(text) } catch { body = null }
  if (!response.ok) {
    throw new CellUnavailable(response.status, typeof (body as { message?: unknown } | null)?.message === "string" ? (body as { message: string }).message : undefined, parseUnavailableDetail(body))
  }
  const parsed = parseAccountScope(body)
  if (parsed === null) throw new StudioScopeError("The server did not report the active workspace scope.")
  const { organization, environmentId } = parsed
  return organization === null || environmentId === null ? null : { orgId: organization.id, environmentId }
}

export function studioScopeQuery() {
  return queryOptions({
    queryKey: ["studio-scope"],
    queryFn: ({ signal }) => readStudioScope(undefined, signal),
    retry: false,
  })
}

export function useStudioScope() {
  return useQuery(studioScopeQuery())
}

/** The currently cached server-reported workspace scope, without triggering a fetch if absent. */
export function useCachedStudioScope(): StudioScope | null {
  const client = use(QueryClientContext)
  return client?.getQueryData<StudioScope | null>(["studio-scope"]) ?? null
}

/**
 * Resolve the originating scope for a mutating host call. Browser callers (`defaultFetch` / `fetch`)
 * fail closed if neither an explicit `boundScope` nor the tab's displayed scope is present, so a
 * write can never silently re-read `/api/me` and adopt a workspace switched in another tab.
 */
export async function resolveWriteScope(fetcher: Fetch, boundScope?: StudioScope | null): Promise<StudioScope | null> {
  if (boundScope !== undefined) return boundScope
  if (currentDisplayedScope !== undefined) return currentDisplayedScope
  if (isDefaultFetch(fetcher)) throw new StudioScopeError("Workspace scope must be loaded before submitting.")
  return readStudioScope(fetcher)
}

/**
 * Every scoped host call carries `orgId` and `environmentId`. Idempotent reads (`GET`/`HEAD`/`OPTIONS`)
 * are bounded by the read deadline (`CELL_FETCH_TIMEOUT_MS` or `init.timeoutMs`) through body
 * completion. Mutating writes (`POST`/`PUT`/`PATCH`/`DELETE`) carry `x-norn-expected-org` and
 * `x-norn-expected-env` bound to the pinned or displayed scope so the Worker refuses stale-tab
 * submissions if the session's active workspace changed elsewhere, and keep their own deadline.
 */
export function scopedHostFetch(fetcher: Fetch, base = "/api/host", scope?: ScopeProvider): (input: string, init?: ScopedRequestInit) => Promise<Response> {
  return async (path, init) => {
    const read = isReadMethod(init?.method)
    const active = scope !== undefined
      ? await scope()
      : read
        ? await readStudioScope(fetcher, init?.signal ?? undefined)
        : await resolveWriteScope(fetcher)
    if (active === null) throw new StudioScopeError(NO_DATA_CELL)
    const [pathname, search] = path.split("?", 2)
    const query = new URLSearchParams(search)
    query.set("orgId", active.orgId)
    query.set("environmentId", active.environmentId)
    const url = `${base}${pathname}?${query}`
    const { timeoutMs, ...requestInit } = init ?? {}
    if (read) {
      return fetchWithTimeout(fetcher, url, init === undefined ? undefined : requestInit, timeoutMs ?? CELL_FETCH_TIMEOUT_MS, () => recordCellReachable(active))
    }
    const headers = new Headers(requestInit.headers)
    headers.set(EXPECTED_ORG_HEADER, active.orgId)
    headers.set(EXPECTED_ENV_HEADER, active.environmentId)
    const response = await fetcher(url, { ...requestInit, headers })
    if (response.ok) recordCellReachable(active)
    return response
  }
}
/** A refusal's findings as the cell wrote them (`diagnostics`: code, message, law and JSON pointer). */
export const findingsOf = (body: unknown): readonly Finding[] => {
  const diagnostics = (body as { diagnostics?: unknown } | null)?.diagnostics
  return Array.isArray(diagnostics) ? diagnostics.map((d: { code?: unknown; message?: unknown; law?: unknown; span?: { pointer?: unknown } | null }) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), law: typeof d.law === "string" ? d.law : null, pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null })) : []
}

/** Thrown when the cell answered with neither a decision nor a refusal it documents: nothing was saved. */
export class CellUnavailable extends Error {
  readonly code: string | undefined
  readonly retryable: boolean | undefined
  readonly explanation: string | undefined
  readonly incidentId: string | undefined
  readonly category: string | undefined
  readonly timedOut: boolean | undefined
  readonly decidedAt: string | undefined

  constructor(readonly status: number, message?: string, detail: UnavailableDetail = {}) {
    // A 404 is a cell without the definition route yet; anything else is a cell that did not answer. Neither saved nor read anything.
    super(message ?? (status === 401 ? "Sign in again." : status === 403 ? "Your role may not read or change these definitions." : status === 404 ? "The cell's definition route isn't connected yet; nothing was read or saved." : `The cell did not answer (${status || "network"}); nothing was read or saved.`))
    this.code = detail.code
    this.retryable = detail.retryable
    this.explanation = detail.explanation
    this.incidentId = detail.incidentId
    this.category = detail.category
    this.timedOut = detail.timedOut
    this.decidedAt = detail.decidedAt
  }
}

/**
 * The cell's definition route, through the Studio Worker's /api/host
 * forwarding as the signed-in account (CELL-DO's contract, 2026-11-06):
 * POST /v1/definitions:admit, :install and /v1/definitions/{id}:retire.
 * GET /v1/definitions:list?kind= (paged) and /v1/definitions/{id} read them
 * back; reading needs Population, so viewers can open dashboards.
 */
export function cellDefinitions(fetcher: Fetch = defaultFetch, base = "/api/host", scope?: ScopeProvider): DefinitionStore {
  const scoped = scopedHostFetch(fetcher, base, scope)
  const post = async (path: string, body: unknown) => {
    let response: Response
    try {
      response = await scoped(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })
    } catch (error) {
      if (error instanceof StudioScopeError) throw error
      if (error instanceof CellUnavailable) throw error
      throw new CellUnavailable(0)
    }
    return { status: response.status, body: await response.json().catch(() => null) as unknown }
  }
  const read = async (path: string) => {
    let response: Response
    try {
      response = await scoped(path, { headers: { accept: "application/json" } })
    } catch (error) {
      if (error instanceof StudioScopeError) throw error
      if (error instanceof CellUnavailable) throw error
      throw new CellUnavailable(0)
    }
    return { status: response.status, body: await response.json().catch(() => null) as unknown }
  }
  const installed = ({ status, body }: { status: number; body: unknown }): Installed => {
    const b = body as { installed?: unknown; id?: unknown; version?: unknown; receipt?: Receipt; code?: unknown } | null
    if (status === 200 && b?.installed === true && typeof b.id === "string" && typeof b.version === "number" && b.receipt) return { installed: true, id: b.id, version: b.version, receipt: b.receipt }
    if (status === 422 && b?.code === "definition.refused") return { installed: false, findings: findingsOf(b) }
    if (status === 409 && b?.code === "definition.version_conflict") return { installed: false, conflict: true }
    if (status === 404) throw new NotConnected()
    const detail = parseUnavailableDetail(body)
    const message = typeof (body as { message?: unknown } | null)?.message === "string" ? (body as { message: string }).message : undefined
    throw new CellUnavailable(status, message, detail)
  }
  return {
    list: async (cursor) => {
      // A custom method: GET /v1/definitions is the catalog read. The cursor is bound to kind (and include).
      const query = new URLSearchParams({ kind: KIND, limit: "50", ...cursor ? { cursor } : {} })
      const { status, body } = await read(`/v1/definitions:list?${query}`)
      const b = body as { items?: unknown; definitions?: unknown; cursor?: unknown; message?: unknown } | null
      const rows = Array.isArray(b?.items) ? b.items : Array.isArray(b?.definitions) ? b.definitions : undefined
      if (status !== 200 || rows === undefined) {
        throw new CellUnavailable(status, status === 404 ? "Dashboards can't be listed yet: the cell has no route to read them back." : typeof b?.message === "string" ? b.message : undefined, parseUnavailableDetail(body))
      }
      return {
        items: rows.map((row: { id: string; title?: string; version: number; lifecycle?: string; updatedAtMs?: number }) => ({ id: row.id, title: row.title ?? "", version: row.version, lifecycle: row.lifecycle ?? "", updatedAtMs: row.updatedAtMs ?? 0 })),
        cursor: typeof b?.cursor === "string" ? b.cursor : null,
      }
    },
    get: async (id) => {
      const { status, body } = await read(`/v1/definitions/${encodeURIComponent(id)}`)
      const b = body as { id?: unknown; kind?: unknown; version?: unknown; payload?: unknown; code?: unknown; message?: unknown } | null
      if (status === 404 && b?.code === "definition.not_found") return undefined
      if (status !== 200 || typeof b?.version !== "number" || !isDashboard(b.payload)) {
        throw new CellUnavailable(status, status === 404 ? "Dashboards can't be reopened yet: the cell has no route to read them back." : typeof b?.message === "string" ? b.message : undefined, parseUnavailableDetail(body))
      }
      return { id: String(b.id ?? id), version: b.version, dashboard: b.payload }
    },
    admit: async (id, dashboard) => {
      const { status, body } = await post("/v1/definitions:admit", { kind: KIND, id, payload: dashboard })
      const b = body as { admitted?: unknown; message?: unknown } | null
      if (status === 404) throw new NotConnected()
      if (status !== 200 || typeof b?.admitted !== "boolean") throw new CellUnavailable(status, typeof b?.message === "string" ? b.message : undefined, parseUnavailableDetail(body))
      return b.admitted ? { admitted: true } : { admitted: false, findings: findingsOf(b) }
    },
    install: async (id, dashboard, expectedVersion) => installed(await post("/v1/definitions:install", { kind: KIND, id, payload: dashboard, ...expectedVersion === undefined ? {} : { expectedVersion } })),
    retire: async (id, expectedVersion) => installed(await post(`/v1/definitions/${encodeURIComponent(id)}:retire`, { expectedVersion })),
  }
}

/** Thrown while the cell's definition route is not connected: nothing was saved. */
export class NotConnected extends Error {
  constructor(message = "Not saved: the cell's definition route isn't connected yet.") {
    super(message)
  }
}

/** The store until CELL-DO's admit and install route lands: every call says nothing was saved or read. */
export const disconnected: DefinitionStore = {
  list: async () => { throw new NotConnected("Dashboards can't be listed: the cell's definition route isn't connected yet.") },
  get: async () => { throw new NotConnected() },
  admit: async () => { throw new NotConnected() },
  install: async () => { throw new NotConnected() },
  retire: async () => { throw new NotConnected() },
}

export const defaultDefinitions: DefinitionStore = cellDefinitions(defaultFetch)
export const DefinitionsContext = createContext<DefinitionStore>(defaultDefinitions)
export const useDefinitions = () => use(DefinitionsContext)

/** A copy of a dashboard under a new title, with fresh tile ids. */
export function duplicateDashboard(dashboard: Dashboard, newId: () => string = () => crypto.randomUUID()): Dashboard {
  return { title: `${dashboard.title} (copy)`, tiles: dashboard.tiles.map((tile) => ({ ...structuredClone(tile), id: newId() })) }
}

/**
 * Where a cell finding on a dashboard belongs in the editor, from its JSON
 * pointer into the definition: the title, one tile, or the dashboard as a whole.
 */
export function dashboardField(pointer: string | null): string | null {
  const parts = (pointer ?? "").split("/").slice(1)
  const at = parts[0] === "payload" ? parts.slice(1) : parts
  if (at[0] === "title") return "title"
  if (at[0] === "tiles" && at[1] !== undefined && /^\d+$/.test(at[1])) return `tile.${at[1]}`
  if (at[0] === "tiles") return "tiles"
  return null
}

export type SaveResult =
  | { readonly saved: { readonly id: string; readonly version: number; readonly receipt: Receipt } }
  | { readonly findings: readonly Finding[] }
  | { readonly conflict: true }

/**
 * Save one dashboard as `id`: the cell admits it, then installs it as the
 * version after `expectedVersion` (a new definition without one). The cell's
 * refusal comes back as findings; a newer version as a conflict.
 */
export async function saveDashboard(store: DefinitionStore, id: string, dashboard: Dashboard, expectedVersion?: number): Promise<SaveResult> {
  const admission = await store.admit(id, dashboard)
  if (!admission.admitted) return { findings: admission.findings }
  const result = await store.install(id, dashboard, expectedVersion)
  if (result.installed) return { saved: { id: result.id, version: result.version, receipt: result.receipt } }
  return "conflict" in result ? { conflict: true } : { findings: result.findings }
}
