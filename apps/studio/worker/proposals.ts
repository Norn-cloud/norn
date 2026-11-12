import type { Answer } from "./model.ts"
import type { Definition, Verdict } from "./agent.ts"

/**
 * The assistant's proposals and spend, in Studio's D1. A proposal is a
 * commitment the person decides: pending (the cell admitted it), refused
 * (the cell refused it), accepted (installed as them) or declined. The cell
 * holds the dashboard itself; a row here is what was asked, proposed, judged
 * and paid for.
 */

export type ProposalState = "pending" | "refused" | "accepted" | "declined"
export interface Proposal {
  readonly id: string
  readonly request: string
  /** The definition id the proposal installs under: a new id, or the dashboard it edits. */
  readonly targetId: string
  readonly targetVersion: number | null
  readonly definition: Definition
  readonly verdict: Verdict & { readonly note: string }
  readonly state: ProposalState
  readonly model: string
  readonly installed: { readonly id: string; readonly version: number } | null
  readonly costMicroUsd: number
  readonly createdAtMs: number
  readonly decidedAtMs: number | null
}
export interface Scope { readonly orgId: string; readonly environmentId: string; readonly userId: string }

interface Row {
  id: string; request: string; target_id: string | null; target_version: number | null; definition: string; verdict: string; state: ProposalState; model: string
  installed_id: string | null; installed_version: number | null; created_at: number; decided_at: number | null; cost: number | null
}
const COLUMNS = `p.id, p.request, p.target_id, p.target_version, p.definition, p.verdict, p.state, p.model, p.installed_id, p.installed_version, p.created_at, p.decided_at,
  (SELECT SUM(s.cost_micro_usd) FROM agent_spend s WHERE s.proposal_id = p.id) AS cost`

const proposalOf = (row: Row): Proposal => ({
  id: row.id,
  request: row.request,
  targetId: row.target_id ?? "",
  targetVersion: row.target_version,
  definition: JSON.parse(row.definition) as Definition,
  verdict: JSON.parse(row.verdict) as Proposal["verdict"],
  state: row.state,
  model: row.model,
  installed: row.installed_id !== null && row.installed_version !== null ? { id: row.installed_id, version: row.installed_version } : null,
  costMicroUsd: row.cost ?? 0,
  createdAtMs: row.created_at,
  decidedAtMs: row.decided_at,
})

/** Everything the assistant has spent, in millionths of a dollar: the cap is checked against this before each call. */
export async function spent(db: D1Database): Promise<number> {
  const row = await db.prepare("SELECT COALESCE(SUM(cost_micro_usd), 0) AS total FROM agent_spend").first<{ total: number }>()
  return row?.total ?? 0
}

/** Record each model call, paid for whether or not it produced a proposal. */
export async function recordSpend(db: D1Database, userId: string, proposalId: string | null, answers: readonly Answer[], now: number): Promise<void> {
  if (answers.length === 0) return
  await db.batch(answers.map((answer) => db.prepare("INSERT INTO agent_spend (id, user_id, proposal_id, model, input_tokens, output_tokens, cost_micro_usd, at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
    .bind(crypto.randomUUID(), userId, proposalId, answer.model, answer.inputTokens, answer.outputTokens, answer.costMicroUsd, now)))
}

export async function insertProposal(db: D1Database, scope: Scope, proposal: Omit<Proposal, "installed" | "decidedAtMs" | "costMicroUsd">): Promise<void> {
  await db.prepare("INSERT INTO dashboard_proposal (id, org_id, environment_id, user_id, request, target_id, target_version, definition, verdict, state, model, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
    .bind(proposal.id, scope.orgId, scope.environmentId, scope.userId, proposal.request, proposal.targetId, proposal.targetVersion, JSON.stringify(proposal.definition), JSON.stringify(proposal.verdict), proposal.state, proposal.model, proposal.createdAtMs)
    .run()
}

/** The person's own proposals in this workspace, newest first. */
export async function listProposals(db: D1Database, scope: Scope, limit = 20): Promise<readonly Proposal[]> {
  const { results } = await db.prepare(`SELECT ${COLUMNS} FROM dashboard_proposal p WHERE p.org_id = ? AND p.environment_id = ? AND p.user_id = ? ORDER BY p.created_at DESC LIMIT ?`)
    .bind(scope.orgId, scope.environmentId, scope.userId, limit).all<Row>()
  return results.map(proposalOf)
}

export async function getProposal(db: D1Database, scope: Scope, id: string): Promise<Proposal | undefined> {
  const { results } = await db.prepare(`SELECT ${COLUMNS} FROM dashboard_proposal p WHERE p.org_id = ? AND p.environment_id = ? AND p.user_id = ? AND p.id = ?`)
    .bind(scope.orgId, scope.environmentId, scope.userId, id).all<Row>()
  const [row] = results
  return row === undefined ? undefined : proposalOf(row)
}

/**
 * Move a pending proposal to its decision. Only a pending row moves (the
 * compare-and-set is in the statement), so a double accept installs once
 * and a decided proposal never changes again.
 */
export async function decide(db: D1Database, scope: Scope, id: string, next: { readonly state: "accepted"; readonly installed: { readonly id: string; readonly version: number } } | { readonly state: "declined" } | { readonly state: "refused"; readonly verdict: Proposal["verdict"] }, now: number): Promise<boolean> {
  const where = "WHERE org_id = ? AND environment_id = ? AND user_id = ? AND id = ? AND state = 'pending'"
  const statement = next.state === "accepted"
    ? db.prepare(`UPDATE dashboard_proposal SET state = 'accepted', installed_id = ?, installed_version = ?, decided_at = ? ${where}`).bind(next.installed.id, next.installed.version, now, scope.orgId, scope.environmentId, scope.userId, id)
    : next.state === "refused"
      ? db.prepare(`UPDATE dashboard_proposal SET state = 'refused', verdict = ?, decided_at = ? ${where}`).bind(JSON.stringify(next.verdict), now, scope.orgId, scope.environmentId, scope.userId, id)
      : db.prepare(`UPDATE dashboard_proposal SET state = 'declined', decided_at = ? ${where}`).bind(now, scope.orgId, scope.environmentId, scope.userId, id)
  const result = await statement.run()
  return (result.meta?.changes ?? 0) > 0
}
