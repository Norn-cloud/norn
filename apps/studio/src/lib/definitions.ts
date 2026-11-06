import { createContext, use } from "react"

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
  readonly view: { readonly kind: "chart" | "table"; readonly chart?: "line" | "bar" | "area" }
  readonly layout: { readonly x: number; readonly y: number; readonly w: number; readonly h: number }
}
export interface Dashboard {
  readonly title: string
  readonly tiles: readonly Tile[]
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

/** A dashboard's stable definition id, from its title when first saved. */
export const dashboardId = (title: string) => `studio.dashboard.${title.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "untitled"}`

const KIND = "dashboard"
type Fetch = (input: string, init?: RequestInit) => Promise<Response>
const findingsOf = (body: unknown): readonly Finding[] => {
  const diagnostics = (body as { diagnostics?: unknown } | null)?.diagnostics
  return Array.isArray(diagnostics) ? diagnostics.map((d: { code?: unknown; message?: unknown; law?: unknown; span?: { pointer?: unknown } | null }) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), law: typeof d.law === "string" ? d.law : null, pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null })) : []
}

/** Thrown when the cell answered with neither a decision nor a refusal it documents: nothing was saved. */
export class CellUnavailable extends Error {
  constructor(status: number, message?: string) {
    super(message ?? (status === 401 ? "Sign in again." : status === 403 ? "Your role may not read or change these definitions." : `The cell did not answer (${status || "network"}); nothing was saved.`))
  }
}

/**
 * The cell's definition route, through the Studio Worker's /api/host
 * forwarding as the signed-in account (CELL-DO's contract, 2026-11-06):
 * POST /v1/definitions:admit, :install and /v1/definitions/{id}:retire.
 * GET /v1/definitions?kind= (paged) and /v1/definitions/{id} read them back.
 */
export function cellDefinitions(fetcher: Fetch = fetch, base = "/api/host"): DefinitionStore {
  const post = async (path: string, body: unknown) => {
    let response: Response
    try {
      response = await fetcher(`${base}${path}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })
    } catch {
      throw new CellUnavailable(0)
    }
    return { status: response.status, body: await response.json().catch(() => null) as unknown }
  }
  const read = async (path: string) => {
    let response: Response
    try {
      response = await fetcher(`${base}${path}`, { headers: { accept: "application/json" } })
    } catch {
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
    throw new CellUnavailable(status)
  }
  return {
    list: async (cursor) => {
      const query = new URLSearchParams({ kind: KIND, ...cursor ? { cursor } : {} })
      const { status, body } = await read(`/v1/definitions?${query}`)
      const b = body as { items?: unknown; definitions?: unknown; cursor?: unknown } | null
      const rows = Array.isArray(b?.items) ? b.items : Array.isArray(b?.definitions) ? b.definitions : undefined
      if (status !== 200 || rows === undefined) throw new CellUnavailable(status, status === 404 ? "Dashboards can't be listed yet: the cell has no route to read them back." : undefined)
      return {
        items: rows.map((row: { id: string; title?: string; version: number; lifecycle?: string; updatedAtMs?: number }) => ({ id: row.id, title: row.title ?? "", version: row.version, lifecycle: row.lifecycle ?? "", updatedAtMs: row.updatedAtMs ?? 0 })),
        cursor: typeof b?.cursor === "string" ? b.cursor : null,
      }
    },
    get: async (id) => {
      const { status, body } = await read(`/v1/definitions/${encodeURIComponent(id)}`)
      const b = body as { id?: unknown; kind?: unknown; version?: unknown; payload?: Dashboard; code?: unknown } | null
      if (status === 404 && b?.code === "definition.not_found") return undefined
      if (status !== 200 || typeof b?.version !== "number" || b.payload === undefined) throw new CellUnavailable(status, status === 404 ? "Dashboards can't be reopened yet: the cell has no route to read them back." : undefined)
      return { id: String(b.id ?? id), version: b.version, dashboard: b.payload }
    },
    admit: async (id, dashboard) => {
      const { status, body } = await post("/v1/definitions:admit", { kind: KIND, id, payload: dashboard })
      const b = body as { admitted?: unknown } | null
      if (status === 404) throw new NotConnected()
      if (status !== 200 || typeof b?.admitted !== "boolean") throw new CellUnavailable(status)
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

export const DefinitionsContext = createContext<DefinitionStore>(cellDefinitions())
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
