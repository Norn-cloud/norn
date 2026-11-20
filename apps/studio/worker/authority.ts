import { SERVICE_ROLES } from "./actor.ts"
import type { Auth } from "./auth.ts"

/**
 * APPROVAL-LIVE-1: the organization's authority for the Norn cell host.
 *
 * When an approver approves a parked action call, the cell runs the call
 * under its requester's CURRENT membership and role, never the role recorded
 * when the call parked. The cell cannot read Studio's membership, and the
 * approver's request carries no token for the requester, so the cell host
 * asks here, service to service: "is subject X a member of organization O
 * now, and with which role?". The answer is a membership attestation this
 * Worker signs with the same keys as host tokens (published at
 * /api/auth/jwks), for its own audience and method, so it never
 * authenticates a request; or 404 `authority.not_member`.
 *
 * Only the cell host calls this, with the shared credential
 * NORN_AUTHORITY_TOKEN (never a NORN_SECRET_* variable, which the cell's
 * connections may name). Without that credential configured the endpoint is
 * not served, and the cell leaves approved calls for the requester's retry.
 */
export const AUTHORITY_PATH = "/api/internal/authority"
/** The audience of an attestation: the cell verifies it apart from host tokens. */
export const AUTHORITY_AUDIENCE = "norn-authority"
/** The method an attestation names; no host token names it. */
export const MEMBERSHIP_METHOD = "membership"
/** An attestation answers one lookup: it lives a minute, so a revocation reaches the cell within it. */
export const ATTESTATION_SECONDS = 60
/** The least credential length accepted; a shorter one leaves the endpoint unserved. */
const MIN_TOKEN_LENGTH = 32

const json = (status: number, body: unknown) => Response.json(body, { status, headers: { "cache-control": "no-store" } })
const refusal = (status: number, code: string, message: string) => json(status, { code, message })

/** Equal by SHA-256 digest, compared without an early exit, so timing reveals nothing of the credential. */
async function sameSecret(presented: string, expected: string): Promise<boolean> {
  const digest = async (text: string) => new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)))
  const [a, b] = await Promise.all([digest(presented), digest(expected)])
  let differ = 0
  for (let index = 0; index < a.length; index++) differ |= (a[index] ?? 0) ^ (b[index] ?? 0)
  return differ === 0
}

/** The role an API key's metadata names (stored as JSON text or an object), or null. */
function keyRole(metadata: unknown): string | null {
  let value = metadata
  if (typeof value === "string") {
    try { value = JSON.parse(value) } catch { return null }
  }
  const role = typeof value === "object" && value !== null ? (value as { role?: unknown }).role ?? "viewer" : "viewer"
  return typeof role === "string" ? role : null
}

/** The membership role `subject` holds in `orgId` now, or null when it holds none. */
async function currentRole(auth: Auth, orgId: string, kind: "user" | "service", id: string, now: number): Promise<string | null> {
  const { adapter } = await auth.$context
  if (kind === "user") {
    const member = await adapter.findOne<{ role: string }>({ model: "member", where: [{ field: "userId", value: id }, { field: "organizationId", value: orgId }] })
    return typeof member?.role === "string" ? member.role : null
  }
  const key = await adapter.findOne<{ referenceId: string; enabled?: boolean | number | null; expiresAt?: Date | string | null; metadata?: unknown }>({ model: "apikey", where: [{ field: "id", value: id }] })
  if (key === null || key.referenceId !== orgId) return null
  if (key.enabled === false || key.enabled === 0) return null
  if (key.expiresAt != null && new Date(key.expiresAt).getTime() <= now * 1000) return null
  const role = keyRole(key.metadata)
  return role !== null && SERVICE_ROLES.some((allowed) => allowed === role) ? role : null
}

/**
 * Answer one lookup: POST `{orgId, subject, kind}` with `Authorization: Bearer <NORN_AUTHORITY_TOKEN>`,
 * where `subject` is the cell's subject (`user:<id>` or `service:<api key id>`). 200 `{token}`: a member
 * now, attested; 404 `authority.not_member`: no member now. Anything else is no answer.
 */
export async function serveAuthority(request: Request, auth: Auth, credential: string | undefined, now = Math.floor(Date.now() / 1000)): Promise<Response> {
  if (credential === undefined || credential.length < MIN_TOKEN_LENGTH) return refusal(404, "route.not_found", "No such endpoint.")
  if (request.method !== "POST") return refusal(405, "route.method_not_allowed", "POST only.")
  const presented = request.headers.get("authorization")?.match(/^Bearer (.+)$/)?.[1]
  if (presented === undefined || !(await sameSecret(presented, credential))) return refusal(401, "authn.required", "The cell host's credential is required.")
  let body: unknown
  try { body = await request.json() } catch { return refusal(400, "request.invalid", "The body is a JSON lookup.") }
  const { orgId, subject, kind } = (typeof body === "object" && body !== null ? body : {}) as Record<string, unknown>
  if (typeof orgId !== "string" || orgId === "" || typeof subject !== "string" || (kind !== "user" && kind !== "service")) return refusal(400, "request.invalid", "A lookup names orgId, subject and kind.")
  const prefix = `${kind}:`
  const id = subject.startsWith(prefix) ? subject.slice(prefix.length) : ""
  if (id === "") return refusal(400, "request.invalid", "The subject is user:<id> or service:<api key id>, matching kind.")
  const role = await currentRole(auth, orgId, kind, id, now)
  if (role === null) return refusal(404, "authority.not_member", "The subject is not a member of the organization now.")
  const { token } = await auth.api.signJWT({
    body: {
      payload: {
        sub: id,
        aud: AUTHORITY_AUDIENCE,
        iat: now,
        exp: now + ATTESTATION_SECONDS,
        norn: { kind, org: orgId, role, amr: MEMBERSHIP_METHOD },
      },
    },
  })
  return json(200, { token })
}
