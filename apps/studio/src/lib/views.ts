import type { Fetch } from "./cell-read.ts"
import { CellUnavailable, readStudioScope, scopedHostFetch, type Finding, type StudioScope } from "./definitions.ts"
import type { Predicate, SortKey } from "./records-query.ts"

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
  /** Admitted actions offered as inline row buttons (only rendered when the cell offers them for the row). */
  readonly rowActions?: readonly string[]
  /** Admitted actions offered on multi-row selection (only run when offered by the cell). */
  readonly bulkActions?: readonly string[]
}

export interface StoredView {
  readonly id: string
  readonly version: number
  readonly updatedAtMs?: number
  readonly view: SavedView
}

/** Suggested Workspace Spaces a view can pin into; custom space names are also accepted. */
export const DEFAULT_SPACES = ["Operations", "Data", "Analysis"] as const

/** What the cell said to a save: the new version, its checker's findings, or that someone saved first. */
export type ViewSaved =
  | { readonly saved: true; readonly id: string; readonly version: number }
  | { readonly saved: false; readonly findings: readonly Finding[] }
  | { readonly saved: false; readonly conflict: true }

export type ViewAdmission =
  | { readonly admitted: true; readonly restatement: string | null }
  | { readonly admitted: false; readonly findings: readonly Finding[] }

const KIND = "view"

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

/** A copy of a view under a new title, preserving its space, filter, sort, columns and actions. */
export function duplicateView(view: SavedView): SavedView {
  return { ...structuredClone(view), title: `${view.title} (copy)` }
}

/** Map a server JSON pointer into a view payload onto its editor `data-field`. */
export function viewField(pointer: string | null): string | null {
  const parts = (pointer ?? "").split("/").slice(1)
  const at = parts[0] === "payload" ? parts.slice(1) : parts
  const head = at[0]
  if (head === "title" || head === "description" || head === "space" || head === "objectType"
    || head === "filter" || head === "sort" || head === "columns" || head === "rowActions" || head === "bulkActions") {
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
  const rowActions = view.rowActions && view.rowActions.length > 0 ? view.rowActions : undefined
  const bulkActions = view.bulkActions && view.bulkActions.length > 0 ? view.bulkActions : undefined
  return {
    title: view.title.trim(),
    ...(description ? { description } : {}),
    ...(space ? { space } : {}),
    objectType: view.objectType,
    ...(filter ? { filter } : {}),
    ...(sort ? { sort } : {}),
    ...(columns ? { columns } : {}),
    ...(rowActions ? { rowActions } : {}),
    ...(bulkActions ? { bulkActions } : {}),
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
export function cellViews(fetcher: Fetch = (input, init) => fetch(input, init)) {
  const callWith = (scopedFetch: Fetch) => async (path: string, init?: RequestInit) => {
    const response = await scopedFetch(path, {
      ...init,
      headers: { accept: "application/json", ...init?.body === undefined ? {} : { "content-type": "application/json" } },
    })
    return { status: response.status, body: await response.json().catch(() => null) as Record<string, unknown> | null }
  }
  const call = callWith(scopedHostFetch(fetcher))
  const saved = ({ status, body }: { status: number; body: Record<string, unknown> | null }): ViewSaved => {
    if (status === 200 && body?.installed === true && typeof body.id === "string" && typeof body.version === "number") {
      return { saved: true, id: body.id, version: body.version }
    }
    if (status === 409 && body?.code === "definition.version_conflict") return { saved: false, conflict: true }
    if (status === 422 && Array.isArray(body?.diagnostics)) return { saved: false, findings: findingsOf(body) }
    throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
  }

  const getWith = (read: typeof call) => async (id: string): Promise<StoredView | undefined> => {
    const { status, body } = await read(`/v1/definitions/${encodeURIComponent(id)}`)
    if (status === 404 && body?.code === "definition.not_found") return undefined
    if (status !== 200 || typeof body?.version !== "number" || !isSavedView(body.payload)) {
      throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
    }
    return {
      id: typeof body.id === "string" ? body.id : id,
      version: body.version,
      ...(typeof body.updatedAtMs === "number" ? { updatedAtMs: body.updatedAtMs } : {}),
      view: body.payload,
    }
  }
  const get = getWith(call)

  const listAll = async (pinnedScope?: StudioScope | null): Promise<readonly StoredView[]> => {
    // Pin one workspace scope for the entire list + per-id fetch sequence so workspace switches
    // cannot mix IDs from one scope with payloads from another.
    const active = pinnedScope !== undefined ? pinnedScope : await readStudioScope(fetcher)
    if (active === null) return []
    const pinnedCall = callWith(scopedHostFetch(fetcher, "/api/host", async () => active))
    const pinnedGet = getWith(pinnedCall)
    const ids: { readonly id: string; readonly updatedAtMs?: number }[] = []
    let cursor: string | null = null
    for (let page = 0; page < 10; page += 1) {
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
    }
    const read = await Promise.all(ids.map(async ({ id, updatedAtMs }) => {
      // Only explicit 404 definition.not_found returns undefined; transport/5xx errors propagate.
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
    /** Read one view by id; `undefined` when not found or retired. */
    get,
    /** Side-effect free server check + plain-words restatement. */
    admit: async (id: string, view: SavedView): Promise<ViewAdmission> => {
      const { status, body } = await call("/v1/definitions:admit", {
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
    save: async (id: string, view: SavedView, expectedVersion?: number): Promise<ViewSaved> =>
      saved(await call("/v1/definitions:install", {
        method: "POST",
        body: JSON.stringify({ kind: KIND, id, payload: cleanView(view), ...expectedVersion === undefined ? {} : { expectedVersion } }),
      })),
    retire: async (id: string, expectedVersion: number): Promise<ViewSaved> =>
      saved(await call(`/v1/definitions/${encodeURIComponent(id)}:retire`, {
        method: "POST",
        body: JSON.stringify({ expectedVersion }),
      })),
  }
}
