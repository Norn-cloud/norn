import { verifyJwsAccessToken } from "better-auth/oauth2"
import { MCP_PATH, ORG_CLAIM, activeMembership, mcpResource, type Auth } from "./auth.ts"
import { hostToken, type NornActor } from "./actor.ts"

/**
 * The MCP endpoint people add to an assistant they already use (Claude, ChatGPT):
 * `https://<studio>/mcp`. Studio is only the edge. It verifies the OAuth access
 * token the person granted that assistant, resolves who they are in which
 * organization exactly as a Studio session is resolved, and forwards the
 * JSON-RPC message to the cell's own `POST /v1/mcp` with a host token. The
 * cell projects the catalog, authorizes and answers every call; nothing here
 * reads data or decides access.
 */
export { MCP_PATH, mcpResource }

/** RFC 9728 metadata for the resource, at the path-suffixed well-known location clients try first. */
export const protectedResourceMetadataUrl = (baseURL: string) => new URL(`/.well-known/oauth-protected-resource${MCP_PATH}`, baseURL).href

/** MCP revisions the cell negotiates; a request naming another is refused here (Streamable HTTP). */
export const PROTOCOL_VERSIONS = new Set(["2025-11-25", "2025-06-18", "2025-03-26"])

/** The largest JSON-RPC message forwarded, in bytes (the cell bounds its own routes too). */
export const MAX_MESSAGE_BYTES = 262_144

const json = (status: number, body: unknown, headers: HeadersInit = {}) =>
  Response.json(body, { status, headers: { "cache-control": "no-store", ...headers } })

const rpcError = (status: number, message: string, headers: HeadersInit = {}) =>
  json(status, { jsonrpc: "2.0", id: null, error: { code: -32000, message } }, headers)

/** 401 with the RFC 9728 pointer: the assistant starts its OAuth flow from here. */
export function challenge(baseURL: string, error?: "invalid_token"): Response {
  const parameters = [`resource_metadata="${protectedResourceMetadataUrl(baseURL)}"`, ...error === undefined ? [] : [`error="${error}"`]]
  return rpcError(401, "Sign in to Norn to use this connector.", { "www-authenticate": `Bearer ${parameters.join(", ")}` })
}

/**
 * The person the token was granted by, in the organization they consented for, with their
 * current role there (read now, not from the token): a removed membership stops working at
 * once. Null when the token names nobody this deployment knows.
 */
export async function grantActor(auth: Auth, claims: { readonly sub?: unknown; readonly [ORG_CLAIM]?: unknown }): Promise<(NornActor & { readonly environmentId: string | null }) | null> {
  if (typeof claims.sub !== "string" || claims.sub === "") return null
  const org = typeof claims[ORG_CLAIM] === "string" && claims[ORG_CLAIM] !== "" ? claims[ORG_CLAIM] : null
  const member = await activeMembership(auth, claims.sub, org)
  if (member === null || (org !== null && member.organizationId !== org)) return null
  return { actorKind: "user", subjectId: claims.sub, orgId: member.organizationId, role: member.role, authMethod: "oauth", environmentId: member.environmentId }
}

/** The access token's claims, verified against this deployment's own signing keys (no network fetch). */
async function verifiedClaims(auth: Auth, baseURL: string, authorization: string | null) {
  const match = /^Bearer ([A-Za-z0-9._~+/-]+=*)$/.exec(authorization ?? "")
  if (match === null) return null
  const token = match[1] as string
  try {
    const claims = await verifyJwsAccessToken(token, {
      jwksFetch: async () => (await auth.api.getJwks()) as never,
      verifyOptions: { issuer: baseURL, audience: mcpResource(baseURL), requiredClaims: ["azp", "sub", "exp"] },
    })
    // A sender-constrained (DPoP) token is not accepted as a plain bearer token.
    return "cnf" in claims ? null : claims
  } catch {
    return null
  }
}

/** Read a body as text, bounded; null once it passes `limit`. */
async function bounded(request: Request, limit: number): Promise<string | null> {
  const reader = request.body?.getReader()
  if (reader === undefined) return ""
  const chunks: Uint8Array[] = []
  let size = 0
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    size += value.byteLength
    if (size > limit) {
      await reader.cancel()
      return null
    }
    chunks.push(value)
  }
  return new Blob(chunks).text()
}

export interface McpEdge {
  readonly auth: Auth
  /** Studio's origin, the token issuer. */
  readonly baseURL: string
  readonly trustedOrigins: readonly string[]
  readonly host: Fetcher | undefined
  /** Studio's built assets, which hold the widget page (dist/widgets/norn.html). */
  readonly assets: Fetcher
}

/**
 * The widgets a chat renders (MCP Apps): one static, data-free page per view, the same file
 * Studio builds, served as `ui://norn/<view>.html`. The cell's tools name these URIs; the
 * page asks for data only through those tools. Like Studio's own /assets, it carries no data
 * and decides nothing, so the edge serves it rather than the cell.
 */
export const WIDGET_VIEWS = ["answer", "lineage", "dashboard", "receipt"] as const
export const UI_MIME_TYPE = "text/html;profile=mcp-app"
export const widgetUri = (view: (typeof WIDGET_VIEWS)[number]) => `ui://norn/${view}.html`
const WIDGET_TITLES: Readonly<Record<(typeof WIDGET_VIEWS)[number], string>> = {
  answer: "Norn answer",
  lineage: "Norn lineage",
  dashboard: "Norn proposal",
  receipt: "Norn action receipt",
}
/** The sandbox the widget needs: inline script and style only, no connections, no frames. */
const WIDGET_META = { ui: { prefersBorder: true, csp: { connectDomains: [], resourceDomains: [] } } }
/** Marks the built widget page, so the single-page fallback is never served as a widget. */
const WIDGET_MARKER = '<meta name="norn-widget" content="1"'

const rpcResult = (id: unknown, result: unknown) => json(200, { jsonrpc: "2.0", id, result })
const rpcFailure = (id: unknown, code: number, message: string, data?: unknown) => json(200, { jsonrpc: "2.0", id, error: { code, message, ...data === undefined ? {} : { data } } })

/** The widget page for one view, naming the view and Studio's origin for its links; null when this build has none. */
export async function widgetPage(edge: Pick<McpEdge, "assets" | "baseURL">, view: (typeof WIDGET_VIEWS)[number]): Promise<string | null> {
  const response = await edge.assets.fetch(new Request(new URL("/widgets/norn.html", edge.baseURL)))
  if (!response.ok) return null
  const html = await response.text()
  if (!html.includes(WIDGET_MARKER) || !html.includes("<head>")) return null
  const origin = new URL(edge.baseURL).origin
  return html.replace("<head>", `<head><meta name="norn-view" content="${view}"><meta name="norn-studio" content="${origin}">`)
}

const RESOURCE_METHODS = new Set(["resources/list", "resources/read", "resources/templates/list"])

/** Answer a resources request for the widget pages; anything else is not a resource this server has. */
async function serveResource(message: { readonly id?: unknown; readonly method: string; readonly params?: { readonly uri?: unknown } }, edge: McpEdge): Promise<Response> {
  if (message.id === undefined) return new Response(null, { status: 202 })
  if (message.method === "resources/templates/list") return rpcResult(message.id, { resourceTemplates: [] })
  if (message.method === "resources/list") {
    return rpcResult(message.id, { resources: WIDGET_VIEWS.map((view) => ({ uri: widgetUri(view), name: `norn-${view}`, title: WIDGET_TITLES[view], mimeType: UI_MIME_TYPE, _meta: WIDGET_META })) })
  }
  const view = WIDGET_VIEWS.find((candidate) => widgetUri(candidate) === message.params?.uri)
  if (view === undefined) return rpcFailure(message.id, -32002, "Resource not found", { uri: message.params?.uri ?? null })
  const page = await widgetPage(edge, view)
  if (page === null) return rpcFailure(message.id, -32002, "This deployment has no widget build", { uri: widgetUri(view) })
  return rpcResult(message.id, { contents: [{ uri: widgetUri(view), mimeType: UI_MIME_TYPE, text: page, _meta: WIDGET_META }] })
}

/** The JSON-RPC message's id, method and params, or null when the body is not one message. */
function parsed(body: string): { readonly id?: unknown; readonly method: string; readonly params?: { readonly uri?: unknown } } | null {
  try {
    const message = JSON.parse(body) as { id?: unknown; method?: unknown; params?: { uri?: unknown } } | null
    return message !== null && typeof message === "object" && typeof message.method === "string" ? { id: message.id, method: message.method, params: message.params } : null
  } catch {
    return null
  }
}

/**
 * One MCP request. Stateless Streamable HTTP: POST carries one JSON-RPC message and gets
 * one JSON reply (or 202 for a notification); there is no event stream to GET or session to DELETE.
 */
export async function serveMcp(request: Request, edge: McpEdge): Promise<Response> {
  if (request.method !== "POST") return rpcError(405, "This MCP server answers POST only; it opens no event stream.", { allow: "POST" })
  // Browsers send Origin; assistants call from their servers without one. A foreign page may not drive it.
  const origin = request.headers.get("origin")
  if (origin !== null && !edge.trustedOrigins.includes(origin)) return rpcError(403, "Cross-origin MCP requests are refused.")
  const version = request.headers.get("mcp-protocol-version")
  if (version !== null && !PROTOCOL_VERSIONS.has(version)) return rpcError(400, `Unsupported MCP-Protocol-Version ${version}.`)
  const claims = await verifiedClaims(edge.auth, edge.baseURL, request.headers.get("authorization"))
  if (claims === null) return challenge(edge.baseURL, request.headers.has("authorization") ? "invalid_token" : undefined)
  const actor = await grantActor(edge.auth, claims as never)
  if (actor === null) return challenge(edge.baseURL, "invalid_token")
  if (actor.environmentId === null) return rpcError(503, "This workspace has no data environment yet.")
  if (edge.host === undefined) return rpcError(503, "No Norn host is connected to this deployment.")
  const body = await bounded(request, MAX_MESSAGE_BYTES)
  if (body === null) return rpcError(413, "The MCP message is too large.")
  const message = parsed(body)
  if (message !== null && RESOURCE_METHODS.has(message.method)) return serveResource(message, edge)
  const scope = new URLSearchParams({ orgId: actor.orgId, environmentId: actor.environmentId })
  const forwarded = await edge.host.fetch(new Request(`https://norn-host/v1/mcp?${scope}`, {
    method: "POST",
    headers: { authorization: `Bearer ${await hostToken(edge.auth, actor)}`, "content-type": "application/json", accept: "application/json" },
    body,
  }))
  if (forwarded.status === 202) return new Response(null, { status: 202 })
  return new Response(forwarded.body, { status: forwarded.status, headers: { "content-type": "application/json", "cache-control": "no-store" } })
}
