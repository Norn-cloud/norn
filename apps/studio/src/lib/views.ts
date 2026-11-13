import type { Fetch } from "./cell-read.ts"
import { CellUnavailable, scopedHostFetch } from "./definitions.ts"
import type { Predicate, SortKey } from "./records-query.ts"

/** A saved records view as the cell keeps it (definition kind `view`, VIEW-DEF-1). */
export interface SavedView {
  readonly title: string
  readonly objectType: string
  readonly filter?: Predicate
  /** The grid's sort, `property:asc|desc` comma separated. */
  readonly sort?: string
  readonly columns?: readonly string[]
}

export interface StoredView { readonly id: string; readonly version: number; readonly view: SavedView }

/** What the cell said to a save: the new version, its checker's findings, or that someone saved first. */
export type ViewSaved =
  | { readonly saved: true; readonly id: string; readonly version: number }
  | { readonly saved: false; readonly findings: readonly { readonly code: string; readonly message: string; readonly pointer: string | null }[] }
  | { readonly saved: false; readonly conflict: true }

const KIND = "view"

/** Every view id for one type starts with this, so a type's views list without reading each one. */
export const viewPrefix = (objectType: string) => `studio.view.${objectType.toLowerCase().replaceAll(/[^a-z0-9]+/g, "_")}.`

/** A new view's id: the type's prefix, the title's words and a short random tail. */
export const newViewId = (objectType: string, title: string, random: () => string = () => crypto.randomUUID().slice(0, 8)) =>
  `${viewPrefix(objectType)}${title.toLowerCase().replaceAll(/[^a-z0-9]+/g, "_").replaceAll(/^_+|_+$/g, "").slice(0, 40) || "view"}_${random()}`

export const sortText = (sort: readonly SortKey[]): string | undefined => sort.length === 0 ? undefined : sort.map(({ property, direction }) => `${property}:${direction}`).join(",")

export function sortKeys(text: string | undefined): readonly SortKey[] {
  return (text ?? "").split(",").filter(Boolean).flatMap((part) => {
    const [property = "", direction] = part.split(":")
    return property !== "" && (direction === "asc" || direction === "desc") ? [{ property, direction }] : []
  })
}

/** The cell's views over one type, through the Studio Worker's host route as the signed-in account. */
export function cellViews(fetcher: Fetch = (input, init) => fetch(input, init)) {
  const scoped = scopedHostFetch(fetcher)
  const call = async (path: string, init?: RequestInit) => {
    const response = await scoped(path, { ...init, headers: { accept: "application/json", ...init?.body === undefined ? {} : { "content-type": "application/json" } } })
    return { status: response.status, body: await response.json().catch(() => null) as Record<string, unknown> | null }
  }
  const saved = ({ status, body }: { status: number; body: Record<string, unknown> | null }): ViewSaved => {
    if (status === 200 && body?.installed === true && typeof body.id === "string" && typeof body.version === "number") return { saved: true, id: body.id, version: body.version }
    if (status === 409 && body?.code === "definition.version_conflict") return { saved: false, conflict: true }
    if (status === 422 && Array.isArray(body?.diagnostics)) {
      return { saved: false, findings: (body.diagnostics as { code?: unknown; message?: unknown; span?: { pointer?: unknown } | null }[]).map((d) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null })) }
    }
    throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
  }
  return {
    /** The type's views, in the cell's id order; retired ones are left out. */
    list: async (objectType: string): Promise<readonly StoredView[]> => {
      const { status, body } = await call(`/v1/definitions:list?${new URLSearchParams({ kind: KIND, limit: "50" })}`)
      const items = Array.isArray(body?.items) ? body.items as { id?: unknown }[] : undefined
      if (status !== 200 || items === undefined) throw new CellUnavailable(status, typeof body?.message === "string" ? body.message : undefined)
      const ids = items.flatMap(({ id }) => typeof id === "string" && id.startsWith(viewPrefix(objectType)) ? [id] : [])
      const read = await Promise.all(ids.map(async (id) => {
        const { status: got, body: document } = await call(`/v1/definitions/${encodeURIComponent(id)}`)
        const view = document?.payload as SavedView | undefined
        return got === 200 && typeof document?.version === "number" && view?.objectType === objectType ? [{ id, version: document.version, view }] : []
      }))
      return read.flat()
    },
    save: async (id: string, view: SavedView, expectedVersion?: number): Promise<ViewSaved> =>
      saved(await call("/v1/definitions:install", { method: "POST", body: JSON.stringify({ kind: KIND, id, payload: view, ...expectedVersion === undefined ? {} : { expectedVersion } }) })),
    retire: async (id: string, expectedVersion: number): Promise<ViewSaved> =>
      saved(await call(`/v1/definitions/${encodeURIComponent(id)}:retire`, { method: "POST", body: JSON.stringify({ expectedVersion }) })),
  }
}
