import { originAllowed, resolveActor, type NornActor } from "./actor.ts"
import type { Auth } from "./auth.ts"
import { Gateway, GatewayError } from "./gateway.ts"

/**
 * `/api/connectors/*` (#772): the signed-in Norn user's tool gateway. The Worker links the user's
 * gateway grant through Better Auth (stored encrypted server-side, never sent to the browser),
 * lists apps and tools with their JSON Schemas, and calls a tool only when the organization's
 * connector policy allows it. Norn stores only that policy; credentials stay in the gateway.
 */

export const GATEWAY_PROVIDER = "executor"

/** Studio capabilities, as declared in `studio.workspace.access` (#984): read, propose (run), approve (policy). */
type Capability = "studio.read" | "studio.propose" | "studio.approve"
const ROLE_CAPABILITIES: Readonly<Record<string, readonly Capability[]>> = {
  viewer: ["studio.read"],
  member: ["studio.read", "studio.propose"],
  admin: ["studio.read", "studio.propose", "studio.approve"],
  owner: ["studio.read", "studio.propose", "studio.approve"],
}
/**
 * The same role table the host's AccessIR policy declares. It stays here only until the host
 * serves connector capabilities from its PDP; then this asks the host instead.
 */
export const permits = (actor: NornActor, capability: Capability) => (ROLE_CAPABILITIES[actor.role] ?? []).includes(capability)

export interface ConnectorEnv {
  /** The gateway origin, e.g. https://gateway.example; absent when this stage has no gateway. */
  readonly GATEWAY_URL?: string
  readonly BETTER_AUTH_URL: string
}

/** What the routes reach outside the request: the policy table, the user's grant and the network. */
export interface ConnectorDeps {
  readonly db: D1Database
  readonly grant?: (auth: Auth, request: Request, actor: NornActor) => Promise<string | null>
  readonly fetch?: typeof fetch
}

/** A typed refusal: `code` is stable, `error` is for people. */
export const refusal = (status: number, code: string, error: string) =>
  Response.json({ code, error }, { status, headers: { "cache-control": "no-store" } })
const ok = (body: unknown) => Response.json(body, { headers: { "cache-control": "no-store" } })

export interface PolicyEntry {
  readonly app: string
  /** A tool name, or `*` for every tool of the app. */
  readonly tool: string
}

export async function policy(db: D1Database, orgId: string): Promise<readonly PolicyEntry[]> {
  const { results } = await db.prepare("SELECT app, tool FROM connector_policy WHERE org_id = ? ORDER BY app, tool").bind(orgId).all<PolicyEntry>()
  return results
}

export const allowed = (entries: readonly PolicyEntry[], app: string, tool: string) =>
  entries.some((entry) => entry.app === app && (entry.tool === tool || entry.tool === "*"))

const NAME = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,199}$/
const isEntry = (value: unknown): value is PolicyEntry => {
  const entry = value as Partial<PolicyEntry> | null
  return typeof entry?.app === "string" && NAME.test(entry.app) && typeof entry.tool === "string" && (entry.tool === "*" || NAME.test(entry.tool))
}

async function replacePolicy(db: D1Database, actor: NornActor, entries: readonly PolicyEntry[]) {
  const now = Date.now()
  await db.batch([
    db.prepare("DELETE FROM connector_policy WHERE org_id = ?").bind(actor.orgId),
    ...entries.map((entry) => db.prepare("INSERT OR IGNORE INTO connector_policy (org_id, app, tool, granted_by, granted_at) VALUES (?, ?, ?, ?, ?)").bind(actor.orgId, entry.app, entry.tool, actor.subjectId, now)),
  ])
}

/** The user's gateway grant, refreshed by Better Auth when it has expired; null when not linked. */
async function grant(auth: Auth, request: Request, actor: NornActor): Promise<string | null> {
  try {
    const { accessToken } = await auth.api.getAccessToken({ body: { providerId: GATEWAY_PROVIDER, userId: actor.subjectId }, headers: request.headers })
    return accessToken ?? null
  } catch {
    return null
  }
}

const gatewayRefusal = (error: unknown) => {
  if (!(error instanceof GatewayError)) throw error
  if (error.tag === "ToolApprovalRequired") return refusal(409, "connectors.approval_required", "The gateway needs this call approved first. Approve it in the gateway, then run it again.")
  if (error.tag === "InputInvalid") return refusal(422, "connectors.input_invalid", error.message)
  if (error.status === 401) return refusal(409, "connectors.not_linked", "Your gateway sign-in has expired. Sign in to the gateway again.")
  if (error.status === 403) return refusal(403, "connectors.gateway_forbidden", error.message)
  if (error.status === 404) return refusal(404, "connectors.not_found", error.message)
  return refusal(502, "connectors.gateway", `The gateway refused (${error.tag}): ${error.message}`)
}

/** Route one `/api/connectors/*` request; `path` is what follows `/api/connectors`. */
export async function connectors(request: Request, env: ConnectorEnv, auth: Auth, path: string, deps: ConnectorDeps): Promise<Response> {
  const db = deps.db
  const actor = await resolveActor(request, auth)
  if (actor === null) return refusal(401, "connectors.sign_in", "Sign in first.")
  // Service keys cannot hold a person's gateway grant.
  if (actor.actorKind !== "user") return refusal(403, "connectors.user_only", "Connectors act for a signed-in person, not an API key.")
  if (!originAllowed(request, actor, env.BETTER_AUTH_URL)) return refusal(403, "connectors.origin", "Cross-origin request refused.")
  if (!permits(actor, "studio.read")) return refusal(403, "connectors.forbidden", "Your role cannot use connectors.")
  const gatewayOrigin = env.GATEWAY_URL
  const route = `${request.method} ${path}`

  if (route === "GET /policy") return ok({ entries: await policy(db, actor.orgId), editable: permits(actor, "studio.approve") })
  if (route === "PUT /policy") {
    if (!permits(actor, "studio.approve")) return refusal(403, "connectors.forbidden", "Only an organization admin can change which tools may be bound.")
    const body = (await request.json().catch(() => null)) as { entries?: unknown } | null
    const entries = body?.entries
    if (!Array.isArray(entries) || entries.length > 500 || !entries.every(isEntry)) return refusal(400, "connectors.invalid_request", "Send {entries: [{app, tool}]}, with at most 500 entries; tool may be \"*\".")
    await replacePolicy(db, actor, entries)
    return ok({ entries: await policy(db, actor.orgId), editable: true })
  }

  if (route === "GET /status" && gatewayOrigin === undefined) return ok({ gateway: null, linked: false })
  if (gatewayOrigin === undefined) return refusal(503, "connectors.not_configured", "No tool gateway is configured for this deployment.")
  if (route === "POST /link") {
    const { url } = await auth.api.oAuth2LinkAccount({ body: { providerId: GATEWAY_PROVIDER, callbackURL: "/connections" }, headers: request.headers })
    return ok({ url })
  }
  const token = await (deps.grant ?? grant)(auth, request, actor)
  if (route === "GET /status") return ok({ gateway: gatewayOrigin, linked: token !== null })
  if (route === "POST /unlink") {
    await auth.api.unlinkAccount({ body: { providerId: GATEWAY_PROVIDER }, headers: request.headers })
    return ok({ linked: false })
  }
  if (token === null) return refusal(409, "connectors.not_linked", "Sign in to the tool gateway first.")
  const gateway = new Gateway(gatewayOrigin, token, deps.fetch ?? fetch)

  try {
    const organization = await gateway.organization()
    if (route === "GET /inventory") return ok(await gateway.inventory(organization))
    const toolsRoute = /^\/apps\/([^/]+)\/tools$/.exec(path)
    if (request.method === "GET" && toolsRoute !== null) {
      const app = decodeURIComponent(toolsRoute[1] ?? "")
      const cursor = new URL(request.url).searchParams.get("cursor") ?? undefined
      const entries = await policy(db, actor.orgId)
      const page = await gateway.tools(organization, app, cursor)
      return ok({ next: page.next, tools: page.tools.map((tool) => ({ ...tool, bindable: allowed(entries, app, tool.name) })) })
    }
    const callRoute = /^\/apps\/([^/]+)\/tools\/([^/]+)\/call$/.exec(path)
    if (request.method === "POST" && callRoute !== null) {
      const app = decodeURIComponent(callRoute[1] ?? "")
      const tool = decodeURIComponent(callRoute[2] ?? "")
      if (!permits(actor, "studio.propose")) return refusal(403, "connectors.forbidden", "Your role can browse tools but not run them.")
      if (!allowed(await policy(db, actor.orgId), app, tool)) return refusal(403, "connectors.tool_not_allowed", `Your organization's connector policy does not allow ${app}.${tool}.`)
      const body = (await request.json().catch(() => null)) as { input?: unknown } | null
      if (body === null || typeof body.input !== "object" || body.input === null || Array.isArray(body.input)) return refusal(400, "connectors.invalid_request", "Send {input: {…}} matching the tool's input schema.")
      return ok({ app, tool, result: await gateway.call(organization, app, tool, body.input) })
    }
  } catch (error) {
    return gatewayRefusal(error)
  }
  return refusal(404, "connectors.not_found", "No such connectors endpoint.")
}
