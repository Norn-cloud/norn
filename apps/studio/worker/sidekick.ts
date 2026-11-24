import { ENV_CLAIM, MEMBER_CLAIM, ORG_CLAIM, mcpResource, type Auth } from "./auth.ts"
import type { NornActor } from "./actor.ts"

/**
 * Sidekick's grant: the signed-in person's own access to Studio's /mcp, for the in-app agent.
 *
 * Studio's Sidekick panel talks to an agent through a relay (Agent Client Protocol). The agent
 * reaches Norn only through Studio's /mcp, and every call must be the person's, so the cell's PDP
 * decides it exactly as for an assistant they connected themselves. This route mints that
 * access for the session that asks: the same kind of token Studio's OAuth server issues to an
 * assistant (issuer this origin, audience `<origin>/mcp`, the workspace claim), with Studio's own
 * first-party client id as `azp` and a short life. /mcp then resolves the person and their
 * current role on every request, so leaving the workspace ends it at once.
 *
 * The token is returned to the page that asked, is never logged, and is not accepted anywhere
 * but /mcp (the cell takes only `norn-host` tokens; Studio's /api takes only the session).
 */

/** The first-party client Sidekick grants name as `azp`; the relay accepts no other. */
export const SIDEKICK_CLIENT = "norn-studio-sidekick"

/** How long one grant lives. The panel renews it while it is open. */
export const GRANT_SECONDS = 10 * 60

/** At most this many grants per person in one window: a panel renews about once in eight minutes. */
export const GRANTS_PER_WINDOW = 20
export const GRANT_WINDOW_MS = 10 * 60_000

/** Grants minted per person, in this isolate (one process on a self-hosted Worker host). */
const minted = new Map<string, number[]>()

/**
 * Whether this person may mint another grant now; records the mint when they may. Keyed by the
 * person, so it bounds all of their sessions together. The map forgets people whose window has passed.
 */
export function grantAllowed(person: string, now = Date.now(), store: Map<string, number[]> = minted): boolean {
  const recent = (store.get(person) ?? []).filter((at) => now - at < GRANT_WINDOW_MS)
  if (recent.length >= GRANTS_PER_WINDOW) {
    store.set(person, recent)
    return false
  }
  store.set(person, [...recent, now])
  if (store.size > 10_000) for (const [key, times] of store) if (times.every((at) => now - at >= GRANT_WINDOW_MS)) store.delete(key)
  return true
}

export interface SidekickGrant {
  readonly token: string
  /** Milliseconds since the epoch. */
  readonly expiresAt: number
}

/** Mint a grant for a signed-in person bound to their active workspace, membership generation, and cell environment. */
export async function sidekickGrant(
  auth: Auth,
  baseURL: string,
  actor: NornActor,
  environmentId: string,
  now = Math.floor(Date.now() / 1000),
): Promise<SidekickGrant> {
  if (typeof actor.memberId !== "string" || actor.memberId === "") {
    throw new Error("Sidekick grant requires a resolved membership.")
  }
  const exp = now + GRANT_SECONDS
  const { token } = await auth.api.signJWT({
    body: {
      payload: {
        iss: baseURL,
        aud: mcpResource(baseURL),
        sub: actor.subjectId,
        azp: SIDEKICK_CLIENT,
        [ORG_CLAIM]: actor.orgId,
        [MEMBER_CLAIM]: actor.memberId,
        [ENV_CLAIM]: environmentId,
        iat: now,
        exp,
      },
    },
  })
  return { token, expiresAt: exp * 1000 }
}
