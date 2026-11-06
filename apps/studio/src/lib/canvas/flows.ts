/**
 * Automations in the cell (D66): a flow definition saved through the definitions routes (DEFS #1078: kind "flow",
 * payload {flow, artifacts, layout?}, the definition id is the flow's id) and run through the run routes (RUNS #1047:
 * POST /v1/runs {flow, runId}). The cell admits, installs, refuses and runs; Studio shows what it says. Every call
 * carries the server-resolved scope (/api/me); without a data cell there is nothing to call.
 */
import type { CanvasEdge, CanvasNode } from "./graph.ts"
import { scopeQuery, type StudioScope } from "../scope.ts"

/** The payload a flow definition installs; `layout` is the drawing, so the automation reopens as it was left. */
export interface FlowPayload {
  readonly flow: Record<string, unknown> & { readonly id: string; readonly version: string }
  readonly artifacts: readonly unknown[]
  readonly layout?: { readonly nodes: readonly CanvasNode[]; readonly edges: readonly CanvasEdge[] }
}
export interface StoredFlow { readonly id: string; readonly version: number; readonly lifecycle: string; readonly payload: FlowPayload }
/** One cell finding: its code (`definition.flow_*` and the checker's), message and where in the payload it points. */
export interface FlowFinding { readonly code: string; readonly message: string; readonly pointer: string | null }
export type FlowAdmission = { readonly admitted: true } | { readonly admitted: false; readonly findings: readonly FlowFinding[] }
export type FlowInstalled =
  | { readonly installed: true; readonly version: number; readonly receipt: string }
  | { readonly installed: false; readonly findings: readonly FlowFinding[] }
  | { readonly installed: false; readonly conflict: true }
  /** Retiring a flow with live runs is refused until they end (`definition.flow_runs_live`). */
  | { readonly installed: false; readonly runsLive: true }
/** One saved automation in the list (no payload: open it to read the drawing). */
export interface FlowSummary { readonly id: string; readonly title: string; readonly version: number; readonly lifecycle: string }
/**
 * The seam for signing off a flow in plain words (D59, as questions do): once DEFS restates flows, this returns the
 * cell's words and their hash for the person to sign, and install sends the hash. Until then there are no words,
 * so there is nothing to sign and nothing is sent.
 */
export type FlowRestatement = { readonly text: string; readonly sha256: string }
export const restateFlow = async (_payload: FlowPayload): Promise<FlowRestatement | undefined> => undefined

/** A started run, as the cell returns it. */
export interface StartedRun { readonly id: string; readonly state: string | null; readonly flow: unknown }

/** Thrown when the cell's routes are not there yet (404 on a route, not a definition): nothing was saved or run. */
export class FlowRoutesMissing extends Error {}
/** Thrown on any answer the contract does not document: nothing was saved or run. */
export class FlowCellError extends Error {
  constructor(readonly status: number, message?: string) {
    super(message ?? (status === 401 ? "Sign in again." : status === 403 ? "Your role may not change or run automations here." : `The cell did not answer (${status || "network"}); nothing was saved or run.`))
  }
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>
const findingsOf = (body: unknown): readonly FlowFinding[] => {
  const diagnostics = (body as { diagnostics?: unknown } | null)?.diagnostics
  return Array.isArray(diagnostics) ? diagnostics.map((d: { code?: unknown; message?: unknown; span?: { pointer?: unknown } | null }) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null })) : []
}

export function flowStore(scope: StudioScope, fetcher: Fetch = (input, init) => fetch(input, init), base = "/api/host") {
  const call = async (path: string, body?: unknown, extra: Record<string, string> = {}) => {
    let response: Response
    try {
      response = await fetcher(`${base}${path}?${scopeQuery(scope, extra)}`, body === undefined ? { headers: { accept: "application/json" } } : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })
    } catch {
      throw new FlowCellError(0)
    }
    return { status: response.status, body: await response.json().catch(() => null) as Record<string, unknown> | null }
  }
  const installed = ({ status, body }: { status: number; body: Record<string, unknown> | null }): FlowInstalled => {
    if (status === 200 && body?.installed === true && typeof body.version === "number") return { installed: true, version: body.version, receipt: String((body.receipt as { receiptHash?: unknown } | undefined)?.receiptHash ?? "") }
    if (status === 422 && body?.code === "definition.refused") return { installed: false, findings: findingsOf(body) }
    if (status === 409 && body?.code === "definition.version_conflict") return { installed: false, conflict: true }
    if (status === 409 && body?.code === "definition.flow_runs_live") return { installed: false, runsLive: true }
    if (status === 404) throw new FlowRoutesMissing("The cell's definition routes aren't live yet; the automation stays a draft in this browser.")
    throw new FlowCellError(status)
  }
  return {
    get: async (id: string): Promise<StoredFlow | undefined> => {
      const { status, body } = await call(`/v1/definitions/${encodeURIComponent(id)}`)
      if (status === 404 && body?.code === "definition.not_found") return undefined
      if (status === 404) throw new FlowRoutesMissing("The cell's definition routes aren't live yet; the automation stays a draft in this browser.")
      if (status !== 200 || body?.kind !== "flow" || typeof body.version !== "number" || typeof body.payload !== "object" || body.payload === null) throw new FlowCellError(status)
      return { id: String(body.id ?? id), version: body.version, lifecycle: String(body.lifecycle ?? "active"), payload: body.payload as FlowPayload }
    },
    admit: async (id: string, payload: FlowPayload): Promise<FlowAdmission> => {
      const { status, body } = await call("/v1/definitions:admit", { kind: "flow", id, payload })
      if (status === 404) throw new FlowRoutesMissing("The cell's definition routes aren't live yet; the automation stays a draft in this browser.")
      if (status !== 200 || typeof body?.admitted !== "boolean") throw new FlowCellError(status)
      return body.admitted ? { admitted: true } : { admitted: false, findings: findingsOf(body) }
    },
    /** A new version of the flow; `expectedVersion` guards against someone saving in between. Flows carry no restatement, so no sign-off (DEFS). */
    /** Every saved automation (kind "flow"), one page at a time. */
    list: async (cursor?: string): Promise<{ readonly items: readonly FlowSummary[]; readonly cursor: string | null }> => {
      const { status, body } = await call("/v1/definitions:list", undefined, { kind: "flow", limit: "50", ...cursor ? { cursor } : {} })
      if (status === 404) throw new FlowRoutesMissing("The cell's definition routes aren't live yet; automations stay drafts in this browser.")
      if (status !== 200 || !Array.isArray(body?.items)) throw new FlowCellError(status)
      return {
        items: (body.items as { id: string; title?: string; version: number; lifecycle?: string }[]).map((row) => ({ id: row.id, title: row.title ?? "", version: row.version, lifecycle: row.lifecycle ?? "" })),
        cursor: typeof body.cursor === "string" ? body.cursor : null,
      }
    },
    install: async (id: string, payload: FlowPayload, expectedVersion?: number, signoff?: string) => installed(await call("/v1/definitions:install", { kind: "flow", id, payload, ...expectedVersion === undefined ? {} : { expectedVersion }, ...signoff === undefined ? {} : { signoff: { restatementSha256: signoff } } })),
    retire: async (id: string, expectedVersion: number) => installed(await call(`/v1/definitions/${encodeURIComponent(id)}:retire`, { expectedVersion })),
    /** Start one run of the installed flow; the same `runId` again returns the same run (an exact retry). */
    start: async (flow: string, runId: string): Promise<StartedRun> => {
      const { status, body } = await call("/v1/runs", { flow, runId })
      if (status === 200 && typeof body?.id === "string") return { id: body.id, state: typeof body.state === "string" ? body.state : null, flow: body.flow }
      if (status === 404 && body?.code === "run.flow_not_found") throw new FlowCellError(404, "The cell has no installed flow by this id; save it first.")
      if (status === 404) throw new FlowRoutesMissing("The cell's run route isn't live yet; nothing was run.")
      throw new FlowCellError(status, typeof body?.message === "string" ? `The cell refused the run: ${body.message}` : undefined)
    },
    read: async (id: string): Promise<StartedRun | undefined> => {
      const { status, body } = await call(`/v1/runs/${encodeURIComponent(id)}`)
      if (status === 404) return undefined
      if (status !== 200 || typeof body?.id !== "string") throw new FlowCellError(status)
      return { id: body.id, state: typeof body.state === "string" ? body.state : null, flow: body.flow }
    },
  }
}
export type FlowStore = ReturnType<typeof flowStore>

/** A fresh run id: readable, unique enough per click, well under the cell's 128 bytes. */
export const newRunId = (now: number = Date.now(), random: () => number = Math.random) => `run_${now.toString(36)}_${Math.floor(random() * 36 ** 6).toString(36).padStart(6, "0")}`

/**
 * Where a cell finding belongs on the canvas: a step the finding names (by id, or by a state it lowered to), else
 * the automation as a whole. `states` maps each step to its generated states.
 */
export function nodeOfFinding(finding: FlowFinding, nodes: readonly string[], states: ReadonlyMap<string, readonly string[]>): string | undefined {
  const text = `${finding.message} ${finding.pointer ?? ""}`
  const named = nodes.find((id) => new RegExp(`(^|[^A-Za-z0-9_])${id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}([^A-Za-z0-9_]|$)`).test(text))
  return named ?? [...states].find(([, generated]) => generated.some((state) => text.includes(state)))?.[0]
}

/**
 * An automation's definition id, minted once from its title when it is created and kept for good (reopening
 * reads it back): `studio.automation.<slug>`, a valid flow id, made unique against the ids already taken.
 */
export function automationId(title: string, taken: readonly string[] = []): string {
  const slug = title.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "").replace(/^(?=[0-9])/, "a_") || "untitled"
  const base = `studio.automation.${slug}`
  if (!taken.includes(base)) return base
  for (let n = 2; ; n += 1) if (!taken.includes(`${base}_${n}`)) return `${base}_${n}`
}
