import { hostToken, type NornActor } from "./actor.ts"
import type { Auth } from "./auth.ts"
import { scopeQuery } from "./scope.ts"

/**
 * Connector permissions, decided by the host's AccessIR/Cedar PDP (#1036). The Worker asks; it
 * holds no role table. The principal is the verified host token's (the host maps it through its
 * declared role table); the Worker never asserts a role, and never supplies a binding's runRole:
 * for `connector.run` the host reads it from the binding's cell definition.
 *
 * Contract: POST /v1/access:decide?orgId=&environmentId=
 *   {checks: [{action, resource: {type, id}}]} → {decisions: [{action, resource, decision, rule}]}
 */

export type ConnectorAction = "connector.read" | "connector.bind" | "connector.run"

export interface Check {
  readonly action: ConnectorAction
  readonly resource: { readonly type: "studio.Workspace" | "studio.ToolBinding"; readonly id: string }
}

export interface Decision extends Check {
  readonly decision: "permit" | "deny"
  /** The policy rule that permitted it; null on a deny. */
  readonly rule: string | null
}

/** The PDP could not be asked, or answered outside the contract: nothing it guards may run (fail closed). */
export class AccessUnavailable extends Error {}

export const workspace = (orgId: string): Check["resource"] => ({ type: "studio.Workspace", id: orgId })
export const toolBinding = (definitionId: string): Check["resource"] => ({ type: "studio.ToolBinding", id: definitionId })

/** Ask the host to decide `checks` for this actor, in one round trip. */
export async function decide(host: Fetcher | undefined, auth: Auth, actor: NornActor, environmentId: string | null, checks: readonly Check[]): Promise<readonly Decision[]> {
  if (host === undefined) throw new AccessUnavailable("No Norn host is connected, so connector permissions cannot be decided.")
  if (environmentId === null) throw new AccessUnavailable("Your organization has no cell environment, so connector permissions cannot be decided.")
  let response: Response
  try {
    response = await host.fetch(new Request(`https://norn-host/v1/access:decide?${scopeQuery({ orgId: actor.orgId, environmentId })}`, {
      method: "POST",
      headers: { authorization: `Bearer ${await hostToken(auth, actor)}`, "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ checks }),
    }))
  } catch {
    throw new AccessUnavailable("The Norn host could not be reached to decide connector permissions.")
  }
  const body = (await response.json().catch(() => null)) as { decisions?: unknown; code?: unknown } | null
  // The shared refusal for a foreign or out-of-scope caller is a deny of everything asked.
  if (response.status === 403) return checks.map((check) => ({ ...check, decision: "deny", rule: null }))
  const decisions = response.status === 200 && Array.isArray(body?.decisions) ? body.decisions as Decision[] : null
  if (decisions === null || decisions.length !== checks.length) throw new AccessUnavailable(`The Norn host answered ${response.status} to a permission check.`)
  // Each answer must be for the check at its position, and a decision the host actually made.
  return checks.map((check, index) => {
    const answer = decisions[index]
    const same = answer?.action === check.action && answer.resource?.type === check.resource.type && answer.resource?.id === check.resource.id
    if (!same || (answer.decision !== "permit" && answer.decision !== "deny")) throw new AccessUnavailable("The Norn host answered a permission check out of contract.")
    return { ...check, decision: answer.decision, rule: typeof answer.rule === "string" ? answer.rule : null }
  })
}
