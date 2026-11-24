import type { Fetch } from "./cell-read.ts"
import { CellUnavailable, defaultFetch, NO_DATA_CELL, readAtScope, readStudioScope, sameStudioScope, scopedHostFetch, StudioScopeError, WORKSPACE_CHANGED_MESSAGE, type Finding, type ScopeProvider, type StudioScope } from "./definitions.ts"
import type { Predicate, RecordsQuery, SortKey } from "./records-query.ts"
import { readRecordsAtScope, type RecordsRead } from "./records-source.ts"

export { WORKSPACE_CHANGED_MESSAGE }

/** A saved records view as the cell keeps it (definition kind `view`, VIEW-DEF-1). */
export interface SavedView {
  readonly title: string
  readonly description?: string
  /** Optional Workspace Space where this view pins in the sidebar. */
  readonly space?: string
  readonly objectType: string
  readonly filter?: Predicate
  /** The grid's sort, `property:asc|desc` comma separated. */
  readonly sort?: string
  readonly columns?: readonly string[]
}

export interface StoredView {
  readonly id: string
  readonly version: number
  /** The workspace scope this view was loaded from; writes refuse if the active scope diverges. */
  readonly scope: StudioScope
  readonly updatedAtMs?: number
  readonly view: SavedView
}

/** Suggested Workspace Spaces a view can pin into; custom space names are also accepted. */
export const DEFAULT_SPACES = ["Operations", "Data", "Analysis"] as const

/** True when two non-null scopes identify the same organization and environment. */
export const sameScope = (a: StudioScope | null | undefined, b: StudioScope | null | undefined): boolean =>
  a != null && b != null && sameStudioScope(a, b)

/** What the cell said to a save: the new version, its checker's findings, or that someone saved first. */
export type ViewSaved =
  | { readonly saved: true; readonly id: string; readonly version: number }
  | { readonly saved: false; readonly findings: readonly Finding[] }
  | { readonly saved: false; readonly conflict: true }

export type ViewAdmission =
  | { readonly admitted: true; readonly restatement: string | null }
  | { readonly admitted: false; readonly findings: readonly Finding[] }

export interface ViewRevision {
  readonly version: number
  readonly retired: boolean
}

export type ViewConflict =
  | { readonly kind: "updated"; readonly version: number }
  | { readonly kind: "deleted"; readonly version: number | null }
  | { readonly kind: "unknown" }

export interface SaveViewDraftOutcome {
  readonly view: SavedView
  readonly result: ViewSaved
  /** Confirmed server state read after a 409 conflict, or null when no conflict occurred. */
  readonly conflict: ViewConflict | null
}

/** Cache key for a saved-view editor's live record preview, bound to its source scope. */
export const viewPreviewQueryKey = (
  scope: StudioScope | null,
  view: Pick<SavedView, "objectType" | "filter" | "sort">,
) => ["view-preview", scope?.orgId, scope?.environmentId, view.objectType, JSON.stringify(view.filter ?? null), view.sort ?? ""] as const

const KIND = "view"
const MAX_TITLE_CHARS = 120
const COPY_SUFFIX = " (copy)"

/** Every view id for one type starts with this, so a type's views list cleanly. */
export const viewPrefix = (objectType: string) => `studio.view.${objectType.toLowerCase().replaceAll(/[^a-z0-9]+/g, "_")}.`

/** A new view's id: the type's prefix, the title's words and a short random tail. */
export const newViewId = (objectType: string, title: string, random: () => string = () => crypto.randomUUID().slice(0, 8)) =>
  `${viewPrefix(objectType)}${title.toLowerCase().replaceAll(/[^a-z0-9]+/g, "_").replaceAll(/^_+|_+$/g, "").slice(0, 40) || "view"}_${random()}`

export const sortText = (sort: readonly SortKey[]): string | undefined =>
  sort.length === 0 ? undefined : sort.map(({ property, direction }) => `${property}:${direction}`).join(",")

export function sortKeys(text: string | undefined): readonly SortKey[] {
  return (text ?? "").split(",").filter(Boolean).flatMap((part) => {
    const [property = "", direction] = part.split(":")
    return property !== "" && (direction === "asc" || direction === "desc") ? [{ property, direction }] : []
  })
}

/** A copy of a view under a new title within the admitted title bound, preserving space, filter, sort and columns. */
export function duplicateView(view: SavedView): SavedView {
  const maxBase = MAX_TITLE_CHARS - COPY_SUFFIX.length
  const base = [...view.title.trim()].slice(0, maxBase).join("").trimEnd()
  return { ...structuredClone(view), title: `${base}${COPY_SUFFIX}` }
}

/** Map a server JSON pointer into a view payload onto its editor `data-field`. */
export function viewField(pointer: string | null): string | null {
  const parts = (pointer ?? "").split("/").slice(1)
  const at = parts[0] === "payload" ? parts.slice(1) : parts
  const head = at[0]
  if (head === "title" || head === "description" || head === "space" || head === "objectType"
    || head === "filter" || head === "sort" || head === "columns") {
    return head
  }
  return null
}

/** Normalize optional fields before sending to the cell so empty strings/arrays don't trigger rejections. */
export function cleanView(view: SavedView): SavedView {
  const description = view.description?.trim()
  const space = view.space?.trim()
  const filter = view.filter && view.filter.all.length > 0 ? view.filter : undefined
  const sort = view.sort?.trim() || undefined
  const columns = view.columns && view.columns.length > 0 ? view.columns : undefined
  return {
    title: view.title.trim(),
    ...(description ? { description } : {}),
    ...(space ? { space } : {}),
    objectType: view.objectType,
    ...(filter ? { filter } : {}),
    ...(sort ? { sort } : {}),
    ...(columns ? { columns } : {}),
  }
}

const findingsOf = (body: Record<string, unknown> | null): readonly Finding[] =>
  Array.isArray(body?.diagnostics)
    ? (body.diagnostics as { code?: unknown; message?: unknown; law?: unknown; span?: { pointer?: unknown } | null }[]).map((d) => ({
      code: String(d.code ?? ""),
      message: String(d.message ?? ""),
      law: typeof d.law === "string" ? d.law : null,
      pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null,
    }))
    : []

const isSavedView = (value: unknown): value is SavedView =>
  value !== null && typeof value === "object" && typeof (value as SavedView).title === "string" && typeof (value as SavedView).objectType === "string"

/** The cell's saved views, through the Studio Worker's host route as the signed-in account. */
export function cellViews(
  fetcher: Fetch = defaultFetch,
  readScope: ScopeProvider = (signal) => readStudioScope(fetcher, signal),
) {
  const callWith = (scopedFetch: Fetch) => async (path: string, init?: RequestInit) => {
    const response = await scopedFetch(path, {
      ...init,
      headers: { accept: "application/json", ...init?.body === undefined ? {} : { "content-type": "application/json" } },
    })
    const text = await response.text()
    let body: Record<string, unknown> | null = null
    if (text !== "") {
      try { body = JSON.parse(text) as Record<string, unknown> | null } catch { body = null }
    }
    return { status: response.status, body }
  }
  const callAt = (scope: StudioScope) => callWith(scopedHostFetch(fetcher, "/api/host", async () => scope))
  const verifiedWriteCall = async (expectedScope: StudioScope | null | undefined) => {
    if (!expectedScope) throw new StudioScopeError(NO_DATA_CELL)
    const active = await readScope()
    if (!sameScope(active, expectedScope)) {
      throw new StudioScopeError(WORKSPACE_CHANGED_MESSAGE)
    }
    return callAt(expectedScope)
  }

  const saved = ({ status, body }: { status: number; body: Record<string, unknown> | null }): ViewSaved => {
    if (status === 200 && body?.installed === true && typeof body.id === "string" && typeof body.version === "number") {
      return { saved: true, id: body.id, version: body.version }
    }
    if (status === 409 && body?.code === "definition.version_conflict") return { saved: false, conflict: true }
    if (status === 422 && Array.isArray(body?.diagnostics)) return { saved: false, findings: findingsOf(body) }
    throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
  }

  const getWith = (read: ReturnType<typeof callWith>, scope: StudioScope) => async (id: string, signal?: AbortSignal): Promise<StoredView | undefined> => {
    const { status, body } = await read(`/v1/definitions/${encodeURIComponent(id)}`, signal === undefined ? undefined : { signal })
    if (status === 404 && body?.code === "definition.not_found") return undefined
    // Only a `view` definition is a view: another kind under this id (an automation, a dashboard)
    // never opens here, so its controls are never wired to this route's retire and save.
    if (status === 200 && body?.kind !== KIND) return undefined
    if (status !== 200 || typeof body?.version !== "number" || !isSavedView(body.payload)) {
      throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
    }
    if (body.lifecycle === "retired") return undefined
    return {
      id: typeof body.id === "string" ? body.id : id,
      version: body.version,
      scope,
      ...(typeof body.updatedAtMs === "number" ? { updatedAtMs: body.updatedAtMs } : {}),
      view: body.payload,
    }
  }

  const get = async (id: string, pinnedScope?: StudioScope | null): Promise<StoredView | undefined> => {
    const active = pinnedScope !== undefined ? pinnedScope : await readScope()
    if (active === null) throw new StudioScopeError(NO_DATA_CELL)
    return getWith(callAt(active), active)(id)
  }

  /**
   * Read the confirmed current revision and lifecycle of a definition (active or retired) in
   * `expectedScope` after checking that the active workspace still matches.
   */
  const currentRevision = async (id: string, expectedScope: StudioScope): Promise<ViewRevision | undefined> => {
    const scopedCall = await verifiedWriteCall(expectedScope)
    const { status, body } = await scopedCall(`/v1/definitions/${encodeURIComponent(id)}`)
    if (status === 404 && body?.code === "definition.not_found") return undefined
    if (status !== 200 || typeof body?.version !== "number") {
      throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
    }
    return { version: body.version, retired: body.lifecycle === "retired" }
  }

  /**
   * Read a page of records bound explicitly to `expectedScope`. Verifies `/api/me` matches both
   * before dispatch (refusing with zero `/api/host` requests on mismatch) and after resolution
   * (discarding any in-flight response that resolves after a scope switch).
   */
  const preview = async (
    objectType: string,
    query: RecordsQuery,
    expectedScope: StudioScope,
    signal?: AbortSignal,
  ): Promise<RecordsRead> => readAtScope(
    expectedScope, readScope, () => readRecordsAtScope(objectType, query, expectedScope, fetcher, signal), signal, WORKSPACE_CHANGED_MESSAGE,
  )

  const listAll = async (pinnedScope?: StudioScope | null): Promise<readonly StoredView[]> => {
    // Pin one workspace scope for the entire list + per-id fetch sequence so workspace switches
    // cannot mix IDs from one scope with payloads from another.
    const active = pinnedScope !== undefined ? pinnedScope : await readScope()
    if (active === null) return []
    const pinnedCall = callAt(active)
    const pinnedGet = getWith(pinnedCall, active)
    const ids: { readonly id: string; readonly updatedAtMs?: number }[] = []
    let cursor: string | null = null
    // Follow the cell's cursor to the end: a list that stopped early would report success while
    // leaving views out. A cursor the cell already sent is a broken listing, never a silent stop.
    const seen = new Set<string>()
    for (;;) {
      const params = new URLSearchParams({ kind: KIND, limit: "50", ...(cursor ? { cursor } : {}) })
      const { status, body } = await pinnedCall(`/v1/definitions:list?${params}`)
      const items = Array.isArray(body?.items) ? body.items as { id?: unknown; updatedAtMs?: unknown }[] : undefined
      if (status !== 200 || items === undefined) {
        throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
      }
      for (const item of items) {
        if (typeof item.id === "string") {
          ids.push({ id: item.id, ...(typeof item.updatedAtMs === "number" ? { updatedAtMs: item.updatedAtMs } : {}) })
        }
      }
      cursor = typeof body?.cursor === "string" ? body.cursor : null
      if (cursor === null) break
      if (seen.has(cursor)) throw new CellUnavailable(status, "The cell's view list repeated a page; not every view could be read.")
      seen.add(cursor)
    }
    const read = await Promise.all(ids.map(async ({ id, updatedAtMs }) => {
      // Only explicit 404 definition.not_found (or retired) returns undefined; transport/5xx errors propagate.
      const stored = await pinnedGet(id)
      return stored === undefined ? [] : [{ ...stored, ...(updatedAtMs !== undefined && stored.updatedAtMs === undefined ? { updatedAtMs } : {}) }]
    }))
    return read.flat()
  }

  return {
    /** All active views across every object type, in the cell's id order; retired ones are left out. */
    listAll,
    /** One object type's active views, in the cell's id order; retired ones are left out. */
    list: async (objectType: string, pinnedScope?: StudioScope | null): Promise<readonly StoredView[]> =>
      (await listAll(pinnedScope)).filter(({ view }) => view.objectType === objectType),
    /** Read one view by id in `pinnedScope` (or current active scope); `undefined` when not found or retired. */
    get,
    /** Read a definition only while its source workspace is active. */
    getAtScope: (id: string, expectedScope: StudioScope, signal?: AbortSignal) =>
      readAtScope(expectedScope, readScope, () => getWith(callAt(expectedScope), expectedScope)(id, signal), signal, WORKSPACE_CHANGED_MESSAGE),
    /** Read the confirmed server revision and lifecycle for conflict handling in `expectedScope`. */
    currentRevision,
    /** Read a record page bound explicitly to `expectedScope`, refusing on workspace switch before or during fetch. */
    preview,
    /** Side-effect free server check + plain-words restatement bound to `expectedScope`. */
    admit: async (id: string, view: SavedView, expectedScope: StudioScope): Promise<ViewAdmission> => {
      const scopedCall = await verifiedWriteCall(expectedScope)
      const { status, body } = await scopedCall("/v1/definitions:admit", {
        method: "POST",
        body: JSON.stringify({ kind: KIND, id, payload: cleanView(view) }),
      })
      if (status !== 200 || typeof body?.admitted !== "boolean") {
        throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
      }
      return body.admitted
        ? { admitted: true, restatement: typeof body.restatement === "string" ? body.restatement : null }
        : { admitted: false, findings: findingsOf(body) }
    },
    /** Install a view in `expectedScope`; refuses before dispatch if the active workspace changed. */
    save: async (id: string, view: SavedView, expectedVersion: number | undefined, expectedScope: StudioScope): Promise<ViewSaved> => {
      const scopedCall = await verifiedWriteCall(expectedScope)
      return saved(await scopedCall("/v1/definitions:install", {
        method: "POST",
        body: JSON.stringify({ kind: KIND, id, payload: cleanView(view), ...expectedVersion === undefined ? {} : { expectedVersion } }),
      }))
    },
    /** Retire a view in `expectedScope`; refuses before dispatch if the active workspace changed. */
    retire: async (id: string, expectedVersion: number, expectedScope: StudioScope): Promise<ViewSaved> => {
      const scopedCall = await verifiedWriteCall(expectedScope)
      return saved(await scopedCall(`/v1/definitions/${encodeURIComponent(id)}:retire`, {
        method: "POST",
        body: JSON.stringify({ expectedVersion }),
      }))
    },
  }
}

export type ViewsClient = ReturnType<typeof cellViews>

/**
 * Save a view draft bound to `scope`. On a 409 conflict for an existing view, confirms the
 * server's current revision and lifecycle in the same scope so an explicit overwrite sends a real
 * compare-and-set `expectedVersion`, a deletion reports `{ kind: "deleted" }`, and an unreadable
 * state withholds overwrite.
 */
export async function saveViewDraft(
  store: Pick<ViewsClient, "save" | "currentRevision">,
  params: {
    readonly id: string
    readonly existing: boolean
    readonly view: SavedView
    readonly expectedVersion: number | undefined
    readonly scope: StudioScope
  },
): Promise<SaveViewDraftOutcome> {
  const result = await store.save(params.id, params.view, params.expectedVersion, params.scope)
  if (!result.saved && "conflict" in result) {
    if (!params.existing) {
      return { view: params.view, result, conflict: { kind: "unknown" } }
    }
    try {
      const latest = await store.currentRevision(params.id, params.scope)
      if (latest === undefined) {
        return { view: params.view, result, conflict: { kind: "deleted", version: null } }
      }
      if (latest.retired) {
        return { view: params.view, result, conflict: { kind: "deleted", version: latest.version } }
      }
      return { view: params.view, result, conflict: { kind: "updated", version: latest.version } }
    } catch (error) {
      if (error instanceof StudioScopeError) throw error
      return { view: params.view, result, conflict: { kind: "unknown" } }
    }
  }
  return { view: params.view, result, conflict: null }
}
