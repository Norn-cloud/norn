import { MAX_OUTPUT_TOKENS, MAX_PROMPT_BYTES, editableDashboard, propose } from "./agent.ts"
import type { Cell } from "./cell.ts"
import { worstCallMicroUsd, type Answer, type Ask, type Model } from "./model.ts"
import { decide, getProposal, insertProposal, listProposals, reserve, reserved, settle, spent, type Proposal, type Scope } from "./proposals.ts"

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
  /** The most one provider call can cost, in millionths of a dollar (default: the bound from the prompt and answer limits). */
  readonly perCallMicroUsd?: number
}

export const MAX_REQUEST_CHARS = 2000
/** Proposals one person may ask for in an hour. */
export const HOURLY_PROPOSALS = 20
/** Model attempts per request (the first, and one repair after a refusal). */
export const ATTEMPTS = 2
/** Provider calls one request may make: each attempt may call the primary and its fallback. */
export const CALLS_PER_REQUEST = ATTEMPTS * 2
/** The default bound on one call's cost: the longest prompt and answer at the most expensive price. */
export const PER_CALL_MICRO_USD = worstCallMicroUsd(MAX_PROMPT_BYTES, MAX_OUTPUT_TOKENS)

const json = (body: unknown, status = 200, headers: Record<string, string> = {}) => Response.json(body, { status, headers: { "cache-control": "no-store", ...headers } })
const refused = (status: number, error: string, headers: Record<string, string> = {}) => json({ error }, status, headers)
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
  // Editing: the dashboard as it stands now, read as the person. Only a dashboard, and only the fields
  // an edit changes, ever reach the model; anything else is refused before any call.
  let target: { readonly id: string; readonly version: number; readonly editable: NonNullable<ReturnType<typeof editableDashboard>> } | undefined
  if (body?.dashboardId !== undefined) {
    if (typeof body.dashboardId !== "string" || body.dashboardId === "") return refused(400, "Name the dashboard to change.")
    const read = await cell.get(`/v1/definitions/${encodeURIComponent(body.dashboardId)}`)
    const doc = read.kind === "answer" && isRecord(read.body) ? read.body : undefined
    if (read.kind !== "answer" || read.status !== 200 || doc === undefined || typeof doc.version !== "number") return refused(read.kind === "answer" && read.status === 403 ? 403 : 404, "That dashboard could not be read.")
    if (doc.kind !== "dashboard") return refused(422, "Only a dashboard can be changed by the assistant.")
    const editable = editableDashboard(doc.payload)
    if (editable === null) return refused(422, "This dashboard has parts the assistant cannot change; edit it by hand.")
    target = { id: body.dashboardId, version: doc.version, editable }
  }
  const proposalId = `prop_${newId()}`
  // Admission: one hourly slot and the request's worst-case cost, reserved atomically before any provider call.
  const perCall = deps.perCallMicroUsd ?? PER_CALL_MICRO_USD
  const admitted = await reserve(db, { requestId: proposalId, userId: scope.userId, amountMicroUsd: perCall * CALLS_PER_REQUEST, capMicroUsd: deps.capMicroUsd, hourly: HOURLY_PROPOSALS, now })
  if (!admitted.ok) {
    return admitted.limit === "proposals_hourly"
      ? refused(429, `At most ${HOURLY_PROPOSALS} proposals an hour; try again later.`, { "x-norn-limit": "proposals_hourly", "retry-after": "3600" })
      : refused(429, "The assistant has reached its spending cap. Build the dashboard by hand, or ask an owner to raise the cap.", { "x-norn-limit": "spend_cap" })
  }
  let answers: readonly Answer[] = []
  try {
    // PRIVACY-GUARD-1: the model never sees personal data. The cell tokenizes the request (markings
    // first, then a detector pass) and keeps the token map; without its guard nothing is sent. The
    // request's own guard is the proposal's receipt; propose() guards everything else at the boundary.
    const guarded = await guardPrompt(cell, asked, proposalId)
    if (guarded === null) {
      await settle(db, { requestId: proposalId, userId: scope.userId, proposalId: null, answers, now })
      return refused(503, "The privacy guard is not available, so nothing was sent to the model.")
    }
    const safe = guarded.safePrompt
    const request_ = target === undefined ? safe : `${safe}\n\nThe dashboard as it stands (change it as asked; keep what the request does not mention):\n${JSON.stringify({ title: target.editable.title, tiles: target.editable.tiles })}`
    let guards = 0
    const boundary = async (text: string) => (await guardPrompt(cell, text, `${proposalId}-b${++guards}`))?.safePrompt ?? null
    const outcome = await propose(cell, model, request_, { proposalId, targetId: target?.id ?? "studio.dashboard.proposal", now, originRequest: safe, attempts: ATTEMPTS, guard: boundary, maxCalls: CALLS_PER_REQUEST })
    answers = outcome.answers
    const { definition, verdict } = outcome
    if (definition === null || verdict === null) {
      await settle(db, { requestId: proposalId, userId: scope.userId, proposalId: null, answers, now })
      const status = outcome.failureKind === "guard" ? 503 : outcome.failureKind === "role" || outcome.failure?.includes("role") ? 403 : outcome.failureKind === "budget" ? 422 : 502
      return refused(status, outcome.failure ?? "The assistant could not propose a dashboard.")
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
    await settle(db, { requestId: proposalId, userId: scope.userId, proposalId, answers, now })
    return json(await getProposal(db, scope, proposalId))
  } catch (error) {
    // A failure after dispatch: what the provider billed is unknown, so the whole reservation is kept
    // as spend. If even this fails, the reservation stays open and keeps counting against the cap.
    await settle(db, { requestId: proposalId, userId: scope.userId, proposalId: null, answers, uncertain: true, now }).catch(() => undefined)
    throw error
  }
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
    if (request.method === "GET") return json({ proposals: await listProposals(deps.db, deps.scope), spentMicroUsd: await spent(deps.db), reservedMicroUsd: await reserved(deps.db), capMicroUsd: deps.capMicroUsd, model: deps.model?.name ?? null })
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
