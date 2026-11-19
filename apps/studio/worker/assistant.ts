import { propose } from "./agent.ts"
import type { Cell } from "./cell.ts"
import type { Answer, Ask, Model } from "./model.ts"
import { decide, getProposal, insertProposal, listProposals, recordSpend, spent, type Proposal, type Scope } from "./proposals.ts"

/**
 * `/api/agent/dashboards*`: the dashboard assistant's routes. The person asks;
 * the server proposes a typed dashboard the cell's checker has already
 * decided; the person accepts (installed as them, signing the checker's
 * words) or declines. The assistant has no right of its own: every cell call
 * is the person's, through the same admission path as a click.
 */

export interface AssistantDeps {
  readonly db: D1Database
  readonly cell: Cell
  readonly scope: Scope
  readonly model: (Model & { answerAll?(ask: Ask): Promise<readonly Answer[]> }) | undefined
  /** The spend cap, in millionths of a dollar. */
  readonly capMicroUsd: number
  readonly now: number
  /** New ids (proposals, dashboards); tests make them deterministic. */
  readonly newId?: () => string
}

export const MAX_REQUEST_CHARS = 2000
/** Proposals one person may ask for in an hour. */
export const HOURLY_PROPOSALS = 20

const json = (body: unknown, status = 200) => Response.json(body, { status, headers: { "cache-control": "no-store" } })
const refused = (status: number, error: string) => json({ error }, status)
const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)

/** A new dashboard's definition id, as Studio names one: its title and a random suffix. */
const dashboardId = (title: string, unique: string) => `studio.dashboard.${title.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "untitled"}_${unique}`

async function proposeRoute(request: Request, deps: AssistantDeps): Promise<Response> {
  const { db, cell, scope, model, now } = deps
  const newId = deps.newId ?? (() => crypto.randomUUID().replaceAll("-", "").slice(0, 12))
  if (model === undefined) return refused(503, "The assistant is not configured on this deployment.")
  const body = await request.json().catch(() => null) as { request?: unknown; dashboardId?: unknown } | null
  const asked = typeof body?.request === "string" ? body.request.trim() : ""
  if (asked === "" || asked.length > MAX_REQUEST_CHARS) return refused(400, `Say what the dashboard should show, in at most ${MAX_REQUEST_CHARS} characters.`)
  if (await spent(db) >= deps.capMicroUsd) return refused(429, "The assistant has reached its spending cap. Build the dashboard by hand, or ask an owner to raise the cap.")
  const recent = await db.prepare("SELECT COUNT(*) AS n FROM dashboard_proposal WHERE user_id = ? AND created_at > ?").bind(scope.userId, now - 3_600_000).first<{ n: number }>()
  if ((recent?.n ?? 0) >= HOURLY_PROPOSALS) return refused(429, `At most ${HOURLY_PROPOSALS} proposals an hour; try again later.`)
  // Editing: the dashboard as it stands now, read as the person, is what the request changes.
  let target: { readonly id: string; readonly version: number; readonly payload: unknown } | undefined
  if (typeof body?.dashboardId === "string") {
    const read = await cell.get(`/v1/definitions/${encodeURIComponent(body.dashboardId)}`)
    const doc = read.kind === "answer" && isRecord(read.body) ? read.body : undefined
    if (read.kind !== "answer" || read.status !== 200 || doc === undefined || typeof doc.version !== "number") return refused(read.kind === "answer" && read.status === 403 ? 403 : 404, "That dashboard could not be read.")
    target = { id: body.dashboardId, version: doc.version, payload: doc.payload }
  }
  const proposalId = `prop_${newId()}`
  // PRIVACY-GUARD-1: the model never sees personal data. The cell tokenizes the request (markings
  // first, then a detector pass) and keeps the token map; without its guard nothing is sent.
  const guarded = await guardPrompt(cell, asked, proposalId)
  if (guarded === null) return refused(503, "The privacy guard is not available, so nothing was sent to the model.")
  const safe = guarded.safePrompt
  const request_ = target === undefined ? safe : `${safe}\n\nThe dashboard as it stands (change it as asked; keep what the request does not mention):\n${JSON.stringify(target.payload)}`
  const outcome = await propose(cell, model, request_, { proposalId, targetId: target?.id ?? "studio.dashboard.proposal", now, originRequest: safe })
  const { definition, verdict } = outcome
  if (definition === null || verdict === null) {
    await recordSpend(db, scope.userId, null, outcome.answers, now)
    return refused(outcome.failure?.includes("role") ? 403 : 502, outcome.failure ?? "The assistant could not propose a dashboard.")
  }
  // The reply comes back through the same guard: restored only where this person may see it.
  const restored = await restoreReply(cell, guarded.guardId, outcome.note, `${proposalId}-note`)
  const guard = { guardId: guarded.guardId, receiptHash: guarded.receiptHash, safePrompt: safe, tokens: guarded.tokens, reply: restored, model: outcome.answers.at(-1)?.model ?? model.name }
  const proposal = {
    // A new dashboard's id follows its proposed title.
    id: proposalId, request: asked, targetId: target?.id ?? dashboardId(definition.title, newId()), targetVersion: target?.version ?? null, definition,
    verdict: { ...verdict, note: outcome.note, guard }, state: verdict.admitted ? "pending" as const : "refused" as const,
    model: outcome.answers.at(-1)?.model ?? model.name, createdAtMs: now,
  }
  await insertProposal(db, scope, proposal)
  await recordSpend(db, scope.userId, proposalId, outcome.answers, now)
  return json(await getProposal(db, scope, proposalId))
}

async function acceptRoute(id: string, deps: AssistantDeps): Promise<Response> {
  const { db, cell, scope, now } = deps
  const proposal = await getProposal(db, scope, id)
  if (proposal === undefined) return refused(404, "No such proposal.")
  if (proposal.state !== "pending") return json({ error: `This proposal is already ${proposal.state}.`, proposal }, 409)
  const install = {
    kind: "dashboard", id: proposal.targetId, payload: proposal.definition,
    ...proposal.targetVersion === null ? {} : { expectedVersion: proposal.targetVersion },
    // Accepting is signing the checker's words for this definition (D59); the cell recomputes them and records the signer.
    ...proposal.verdict.restatementSha256 === null ? {} : { signoff: { restatementSha256: proposal.verdict.restatementSha256 } },
  }
  const answer = await cell.post("/v1/definitions:install", install)
  if (answer.kind === "no-cell") return refused(409, "This workspace has no data cell yet.")
  const body = isRecord(answer.body) ? answer.body : {}
  if (answer.status === 200 && body.installed === true && typeof body.id === "string" && typeof body.version === "number") {
    await decide(db, scope, id, { state: "accepted", installed: { id: body.id, version: body.version } }, now)
    return json(await getProposal(db, scope, id))
  }
  if (answer.status === 409) return json({ error: "The dashboard changed since this proposal was made; ask again.", proposal }, 409)
  if (answer.status === 422) {
    const findings = (Array.isArray(body.diagnostics) ? body.diagnostics : []).map((d: { code?: unknown; message?: unknown; span?: { pointer?: unknown } | null }) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null }))
    const verdict = { ...proposal.verdict, admitted: false, findings: findings.length > 0 ? findings : [{ code: String(body.code ?? "definition.refused"), message: String(body.message ?? "The cell refused this dashboard."), pointer: null }] }
    await decide(db, scope, id, { state: "refused", verdict }, now)
    return json(await getProposal(db, scope, id))
  }
  if (answer.status === 403) return refused(403, "Your role may not create dashboards here.")
  return refused(502, `The cell did not answer (${answer.status}).`)
}

/** A guarded prompt as the cell returns it: tokens and receipt, never a value. */
export interface Guarded {
  readonly guardId: number
  readonly receiptHash: string
  readonly safePrompt: string
  readonly tokens: readonly { readonly token: string; readonly kind: string; readonly label: string; readonly source: string; readonly revealable: boolean }[]
}
export interface RestoredSegment { readonly text: string; readonly token?: string; readonly kind?: string; readonly state?: "restored" | "projected" | "kept" }

/** The cell's guard for one model call, as the person; null when the cell has none (then nothing is sent). */
export async function guardPrompt(cell: Cell, text: string, key: string): Promise<Guarded | null> {
  const answer = await cell.post("/v1/guard:prompt", { text, purpose: "dashboard-assistant", idempotencyKey: key })
  if (answer.kind !== "answer" || answer.status !== 200 || !isRecord(answer.body) || typeof answer.body.safePrompt !== "string" || typeof answer.body.guardId !== "number") return null
  return answer.body as unknown as Guarded
}

/** A model reply restored by the cell for the person; the reply as sent when the cell cannot restore it. */
export async function restoreReply(cell: Cell, guardId: number, text: string, key: string): Promise<readonly RestoredSegment[]> {
  if (text === "") return []
  const answer = await cell.post(`/v1/guard/${guardId}:restore`, { text, idempotencyKey: key })
  const segments = answer.kind === "answer" && answer.status === 200 && isRecord(answer.body) && Array.isArray(answer.body.segments) ? answer.body.segments as RestoredSegment[] : null
  return segments ?? [{ text }]
}

/** One `/api/agent/dashboards*` request, resolved to its person and their cell. */
export async function assistant(request: Request, route: string, deps: AssistantDeps): Promise<Response> {
  if (route === "" || route === "/") {
    if (request.method === "GET") return json({ proposals: await listProposals(deps.db, deps.scope), spentMicroUsd: await spent(deps.db), capMicroUsd: deps.capMicroUsd, model: deps.model?.name ?? null })
    if (request.method === "POST") return proposeRoute(request, deps)
    return refused(405, "Method not allowed.")
  }
  const match = /^\/(prop_[a-z0-9]+)(?:\/(accept|decline))?$/.exec(route)
  if (match === null) return refused(404, "No such endpoint.")
  const [, id = "", verb] = match
  if (verb === undefined) {
    if (request.method !== "GET") return refused(405, "Method not allowed.")
    const proposal = await getProposal(deps.db, deps.scope, id)
    return proposal === undefined ? refused(404, "No such proposal.") : json(proposal)
  }
  if (request.method !== "POST") return refused(405, "Method not allowed.")
  if (verb === "accept") return acceptRoute(id, deps)
  const proposal = await getProposal(deps.db, deps.scope, id)
  if (proposal === undefined) return refused(404, "No such proposal.")
  if (!(await decide(deps.db, deps.scope, id, { state: "declined" }, deps.now))) return json({ error: `This proposal is already ${proposal.state}.`, proposal }, 409)
  return json(await getProposal(deps.db, deps.scope, id))
}

export type { Proposal }
