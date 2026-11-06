/**
 * Canvas definitions saved in the cell: the same definition routes as
 * dashboards (`lib/definitions.ts`, CELL-DO's contract), for any `kind`.
 * The cell checks every admit and install and decides; a refusal names the law.
 */
import { CellUnavailable, NotConnected, type Admission, type Finding, type Receipt } from "../definitions.ts"

export interface Summary { readonly id: string; readonly title: string; readonly version: number; readonly lifecycle: string; readonly updatedAtMs: number }
export interface Stored<T> { readonly id: string; readonly version: number; readonly payload: T }
export type Saved =
  | { readonly installed: true; readonly id: string; readonly version: number; readonly receipt: Receipt }
  | { readonly installed: false; readonly findings: readonly Finding[] }
  | { readonly installed: false; readonly conflict: true }

export interface KindStore<T> {
  list(cursor?: string | null): Promise<{ readonly items: readonly Summary[]; readonly cursor: string | null }>
  get(id: string): Promise<Stored<T> | undefined>
  admit(id: string, payload: T): Promise<Admission>
  install(id: string, payload: T, expectedVersion?: number): Promise<Saved>
  retire(id: string, expectedVersion: number): Promise<Saved>
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

const findingsOf = (body: unknown): readonly Finding[] => {
  const diagnostics = (body as { diagnostics?: unknown } | null)?.diagnostics
  return Array.isArray(diagnostics)
    ? diagnostics.map((d: { code?: unknown; message?: unknown; law?: unknown; span?: { pointer?: unknown } | null }) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), law: typeof d.law === "string" ? d.law : null, pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null }))
    : []
}

/** The definition routes for one kind, through the Studio Worker's /api/host forwarding as the signed-in account. */
export function kindStore<T>(kind: string, fetcher: Fetch = (input, init) => fetch(input, init), base = "/api/host"): KindStore<T> {
  const call = async (path: string, body?: unknown) => {
    let response: Response
    try {
      response = await fetcher(`${base}${path}`, body === undefined ? { headers: { accept: "application/json" } } : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })
    } catch {
      throw new CellUnavailable(0)
    }
    return { status: response.status, body: await response.json().catch(() => null) as unknown }
  }
  const saved = ({ status, body }: { status: number; body: unknown }): Saved => {
    const b = body as { installed?: unknown; id?: unknown; version?: unknown; receipt?: Receipt; code?: unknown } | null
    if (status === 200 && b?.installed === true && typeof b.id === "string" && typeof b.version === "number" && b.receipt) return { installed: true, id: b.id, version: b.version, receipt: b.receipt }
    if (status === 422 && b?.code === "definition.refused") return { installed: false, findings: findingsOf(b) }
    if (status === 409 && b?.code === "definition.version_conflict") return { installed: false, conflict: true }
    if (status === 404) throw new NotConnected()
    throw new CellUnavailable(status)
  }
  return {
    list: async (cursor) => {
      const { status, body } = await call(`/v1/definitions:list?${new URLSearchParams({ kind, limit: "50", ...cursor ? { cursor } : {} })}`)
      const b = body as { items?: unknown; cursor?: unknown } | null
      if (status === 404) throw new NotConnected(`The cell's definition routes aren't live yet, so ${kind} definitions can't be listed or saved there.`)
      if (status !== 200 || !Array.isArray(b?.items)) throw new CellUnavailable(status)
      return {
        items: b.items.map((row: { id: string; title?: string; version: number; lifecycle?: string; updatedAtMs?: number }) => ({ id: row.id, title: row.title ?? "", version: row.version, lifecycle: row.lifecycle ?? "", updatedAtMs: row.updatedAtMs ?? 0 })),
        cursor: typeof b.cursor === "string" ? b.cursor : null,
      }
    },
    get: async (id) => {
      const { status, body } = await call(`/v1/definitions/${encodeURIComponent(id)}`)
      const b = body as { id?: unknown; kind?: unknown; version?: unknown; payload?: T; code?: unknown } | null
      if (status === 404 && b?.code === "definition.not_found") return undefined
      if (status === 404) throw new NotConnected(`The cell's definition routes aren't live yet, so this ${kind} can only be a draft in this browser.`)
      if (status !== 200 || typeof b?.version !== "number" || b.payload === undefined || b.kind !== kind) throw new CellUnavailable(status)
      return { id: String(b.id ?? id), version: b.version, payload: b.payload }
    },
    admit: async (id, payload) => {
      const { status, body } = await call("/v1/definitions:admit", { kind, id, payload })
      const b = body as { admitted?: unknown } | null
      if (status === 404) throw new NotConnected()
      if (status !== 200 || typeof b?.admitted !== "boolean") throw new CellUnavailable(status)
      return b.admitted ? { admitted: true } : { admitted: false, findings: findingsOf(b) }
    },
    install: async (id, payload, expectedVersion) => saved(await call("/v1/definitions:install", { kind, id, payload, ...expectedVersion === undefined ? {} : { expectedVersion } })),
    retire: async (id, expectedVersion) => saved(await call(`/v1/definitions/${encodeURIComponent(id)}:retire`, { expectedVersion })),
  }
}
