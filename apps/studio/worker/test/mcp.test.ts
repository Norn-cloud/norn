import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { createHash, randomBytes } from "node:crypto"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { assistantCallback, cimdAllowed, CONSENT_APPROVAL_HEADER, createAuth, encodeGrantRef, fetchClientMetadata, MEMBER_CLAIM, ORG_CLAIM } from "../auth.ts"
import worker, { type Env } from "../index.ts"
import { EXPECTED_ORG_HEADER, hostToken } from "../actor.ts"
import { grantActor, retried } from "../mcp.ts"

/**
 * An assistant connecting to Norn end to end, as Claude and ChatGPT do it: discovery, client
 * registration, the person's sign-in and consent, the code exchange, then MCP calls that reach
 * the cell as that person with a host token. The cell is a stub here; what it receives is checked.
 */

const ORIGIN = "https://studio-alpha.norn.test"
const RESOURCE = `${ORIGIN}/mcp`
const CLAUDE = "https://claude.ai/api/mcp/auth_callback"
const migration = (name: string) => readFileSync(join(import.meta.dir, "../migrations", name), "utf8")

interface Forwarded { readonly url: string; readonly authorization: string | null; readonly body: string }

function stage(mcp = true): Env & { forwarded: Forwarded[] } {
  const db = new Database(":memory:")
  for (const name of ["0001_better_auth.sql", "0002_workspace_provisioning.sql", "0007_oauth_provider.sql", "0013_oauth_membership_guards.sql"]) db.exec(migration(name))
  const assets = { fetch: async () => new Response("<!doctype html>studio") } as unknown as Fetcher
  const forwarded: Forwarded[] = []
  const host = {
    fetch: async (request: Request) => {
      const body = await request.text()
      forwarded.push({ url: request.url, authorization: request.headers.get("authorization"), body })
      const message = JSON.parse(body) as { id?: number }
      if (message.id === undefined) return new Response(null, { status: 202 })
      return Response.json({ jsonrpc: "2.0", id: message.id, result: { served: true } })
    },
  } as unknown as Fetcher
  return { ASSETS: assets, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip", NORN_HOST: host, ...mcp ? { STUDIO_MCP: "on" } : {}, forwarded }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  if (!headers.has("cf-connecting-ip")) headers.set("cf-connecting-ip", `203.0.113.${++address % 250}`)
  return worker.fetch(new Request(path.startsWith("http") ? path : `${ORIGIN}${path}`, { redirect: "manual", ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const postJson = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
  call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", accept: "application/json", ...headers } })
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
const db = (env: Env) => env.AUTH_DB as unknown as Database

/** A person with a workspace whose data environment is `analytics`; returns their session cookie and ids. */
async function person(env: Env, email: string) {
  await postJson(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name: email }, { origin: ORIGIN })
  const signIn = await postJson(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" }, { origin: ORIGIN })
  expect(signIn.status).toBe(200)
  const user = (db(env).query("select id from user where email = ?").get(email) as { id: string }).id
  const org = (db(env).query("select organizationId from member where userId = ?").get(user) as { organizationId: string }).organizationId
  const metadata = JSON.parse((db(env).query("select metadata from organization where id = ?").get(org) as { metadata: string }).metadata)
  db(env).run("update organization set metadata = ? where id = ?", [JSON.stringify({ ...metadata, environmentId: "analytics" }), org])
  return { cookie: cookieOf(signIn), user, org }
}

/** Dynamic client registration as an assistant's server sends it: no session, no Origin. */
async function register(env: Env, redirect = CLAUDE): Promise<string> {
  const response = await postJson(env, "/api/auth/oauth2/register", { client_name: "Claude", redirect_uris: [redirect], token_endpoint_auth_method: "none", grant_types: ["authorization_code", "refresh_token"], response_types: ["code"] })
  expect(response.status).toBeLessThan(300)
  return ((await response.json()) as { client_id: string }).client_id
}

const base64url = (bytes: Buffer) => bytes.toString("base64url")
function pkce() {
  const verifier = base64url(randomBytes(32))
  return { verifier, challenge: base64url(createHash("sha256").update(verifier).digest()) }
}

function authorizeUrl(clientId: string, challenge: string, redirect = CLAUDE) {
  return `/api/auth/oauth2/authorize?${new URLSearchParams({ response_type: "code", client_id: clientId, redirect_uri: redirect, scope: "openid offline_access", state: "st-1", code_challenge: challenge, code_challenge_method: "S256", resource: RESOURCE })}`
}

/** The signed query a redirect hands to Studio's sign-in or consent page. */
const signedQueryOf = (location: string) => new URL(location, ORIGIN).search.slice(1)

/** Fetch the consent screen context and mint its single-use approval token for `oauthQuery`. */
async function consentContext(env: Env, cookie: string, oauthQuery: string) {
  const response = await postJson(env, "/api/oauth/consent-context", { oauth_query: oauthQuery }, { cookie, origin: ORIGIN })
  expect(response.status).toBe(200)
  return (await response.json()) as { user: { email: string }; organization: { id: string; name: string } | null; approvalToken: string | null }
}

/** Sign in, consent and exchange the code: the access token an assistant ends up holding. */
async function grant(env: Env, cookie: string, expectedOrg?: string): Promise<{ accessToken: string; refreshToken?: string; claims: Record<string, unknown> }> {
  const clientId = await register(env)
  const { verifier, challenge } = pkce()
  const authorize = await call(env, authorizeUrl(clientId, challenge), { headers: { cookie } })
  expect(authorize.status).toBe(302)
  const consentPage = authorize.headers.get("location") ?? ""
  expect(new URL(consentPage, ORIGIN).pathname).toBe("/oauth/consent")
  const oauthQuery = signedQueryOf(consentPage)
  const ctx = await consentContext(env, cookie, oauthQuery)
  const org = expectedOrg ?? ctx.organization?.id ?? ""
  expect(ctx.approvalToken).toBeString()
  const consent = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctx.approvalToken ?? "" },
  )
  expect(consent.status).toBe(200)
  const callback = new URL(((await consent.json()) as { url: string }).url)
  expect(`${callback.origin}${callback.pathname}`).toBe(CLAUDE)
  expect(callback.searchParams.get("state")).toBe("st-1")
  expect(callback.searchParams.get("iss")).toBe(ORIGIN)
  const token = await call(env, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: new URLSearchParams({ grant_type: "authorization_code", code: callback.searchParams.get("code") ?? "", redirect_uri: CLAUDE, client_id: clientId, code_verifier: verifier, resource: RESOURCE }),
  })
  expect(token.status).toBe(200)
  const payload = (await token.json()) as { access_token: string; refresh_token?: string }
  return {
    accessToken: payload.access_token,
    refreshToken: payload.refresh_token,
    claims: JSON.parse(Buffer.from(payload.access_token.split(".")[1] ?? "", "base64url").toString()),
  }
}

const mcp = (env: Env, token: string | null, message: unknown, headers: HeadersInit = {}) =>
  call(env, "/mcp", { method: "POST", body: JSON.stringify(message), headers: { "content-type": "application/json", accept: "application/json, text/event-stream", ...token === null ? {} : { authorization: `Bearer ${token}` }, ...headers } })
const INIT = { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "test", version: "0" } } }

test("discovery names this origin as the resource's only authorization server", async () => {
  const env = stage()
  const resource = await call(env, "/.well-known/oauth-protected-resource/mcp")
  expect(resource.status).toBe(200)
  const prm = (await resource.json()) as { resource: string; authorization_servers: string[] }
  expect(prm.resource).toBe(RESOURCE)
  expect(prm.authorization_servers).toEqual([ORIGIN])
  const server = await call(env, "/.well-known/oauth-authorization-server")
  expect(server.status).toBe(200)
  const metadata = (await server.json()) as Record<string, unknown>
  expect(metadata.issuer).toBe(ORIGIN)
  expect(metadata.code_challenge_methods_supported).toContain("S256")
  expect(metadata.token_endpoint_auth_methods_supported).toContain("none")
  expect(typeof metadata.registration_endpoint).toBe("string")
  expect(metadata.client_id_metadata_document_supported).toBe(true)
  expect(metadata.authorization_response_iss_parameter_supported).toBe(true)
})

test("an unauthenticated MCP request is challenged toward that metadata", async () => {
  const env = stage()
  const response = await mcp(env, null, INIT)
  expect(response.status).toBe(401)
  expect(response.headers.get("www-authenticate")).toContain(`resource_metadata="${ORIGIN}/.well-known/oauth-protected-resource/mcp"`)
  expect(env.forwarded).toHaveLength(0)
})

test("a granted assistant reaches the cell as that person, in the workspace they consented for", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "ada@example.com")
  const { accessToken, claims } = await grant(env, cookie)
  expect(claims.aud === RESOURCE || (Array.isArray(claims.aud) && claims.aud.includes(RESOURCE))).toBe(true)
  expect(claims["https://norn.cloud/org"]).toBe(org)

  const response = await mcp(env, accessToken, INIT)
  expect(response.status).toBe(200)
  expect((await response.json()) as Record<string, unknown>).toEqual({ jsonrpc: "2.0", id: 1, result: { served: true } })
  const [sent] = env.forwarded
  expect(new URL(sent?.url ?? "").pathname).toBe("/v1/mcp")
  expect(Object.fromEntries(new URL(sent?.url ?? "").searchParams)).toEqual({ orgId: org, environmentId: "analytics" })
  expect(JSON.parse(sent?.body ?? "")).toEqual(INIT)
  // The cell sees a host token for the person (never the assistant's token), signed in by grant.
  const hostClaims = JSON.parse(Buffer.from((sent?.authorization ?? "").split(".")[1] ?? "", "base64url").toString())
  expect(hostClaims).toMatchObject({ sub: user, aud: "norn-host", norn: { kind: "user", org, role: "owner", amr: "oauth" } })

  const notification = await mcp(env, accessToken, { jsonrpc: "2.0", method: "notifications/initialized" })
  expect(notification.status).toBe(202)

  // Leaving the workspace ends the grant at once: membership is read on every request.
  db(env).run("delete from member where userId = ?", [user])
  expect((await mcp(env, accessToken, INIT)).status).toBe(401)
})

test("signing in from the assistant's flow resumes it at consent", async () => {
  const env = stage()
  await person(env, "bo@example.com")
  const clientId = await register(env)
  const { challenge } = pkce()
  const authorize = await call(env, authorizeUrl(clientId, challenge))
  expect(authorize.status).toBe(302)
  const signInPage = authorize.headers.get("location") ?? ""
  expect(new URL(signInPage, ORIGIN).pathname).toBe("/oauth/sign-in")
  const signIn = await postJson(env, "/api/auth/sign-in/email", { email: "bo@example.com", password: "correct horse battery", oauth_query: signedQueryOf(signInPage) }, { origin: ORIGIN })
  expect(signIn.status).toBe(200)
  expect(new URL(((await signIn.json()) as { url: string }).url, ORIGIN).pathname).toBe("/oauth/consent")
})

test("a loopback callback registers without naming an application type, and gets a code", async () => {
  const env = stage()
  const { cookie, org } = await person(env, "lo@example.com")
  const loopback = "http://127.0.0.1:33418/callback"
  const clientId = await register(env, loopback)
  const authorize = await call(env, authorizeUrl(clientId, pkce().challenge, loopback), { headers: { cookie } })
  expect(authorize.status).toBe(302)
  const consentPage = authorize.headers.get("location") ?? ""
  expect(new URL(consentPage, ORIGIN).pathname).toBe("/oauth/consent")
  const oauthQuery = signedQueryOf(consentPage)
  const ctx = await consentContext(env, cookie, oauthQuery)
  const consent = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctx.approvalToken ?? "" },
  )
  expect(consent.status).toBe(200)
  const callback = new URL(((await consent.json()) as { url: string }).url)
  expect(`${callback.origin}${callback.pathname}`).toBe(loopback)
  expect(callback.searchParams.get("code")).toBeTruthy()
})

test("a code is never sent to a callback outside the known assistants", async () => {
  const env = stage()
  const { cookie } = await person(env, "cy@example.com")
  const evil = "https://evil.example/callback"
  const response = await postJson(env, "/api/auth/oauth2/register", { client_name: "Claude", redirect_uris: [evil], token_endpoint_auth_method: "none", grant_types: ["authorization_code"], response_types: ["code"] })
  if (response.status < 300) {
    const clientId = ((await response.json()) as { client_id: string }).client_id
    const authorize = await call(env, authorizeUrl(clientId, pkce().challenge, evil), { headers: { cookie } })
    expect(authorize.headers.get("location") ?? "").not.toStartWith(evil)
  }
})

test("tokens for another audience, foreign origins and other methods are refused", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "di@example.com")
  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, mcp: true })
  // A host token is signed by the same keys but for the cell, not for /mcp.
  const forCell = await hostToken(auth, { actorKind: "user", subjectId: user, orgId: org, role: "owner", authMethod: "password" })
  expect((await mcp(env, forCell, INIT)).status).toBe(401)
  const { accessToken } = await grant(env, cookie)
  expect((await mcp(env, accessToken, INIT, { origin: "https://evil.example" })).status).toBe(403)
  expect((await mcp(env, accessToken, INIT, { "mcp-protocol-version": "1999-01-01" })).status).toBe(400)
  expect((await call(env, "/mcp", { headers: { authorization: `Bearer ${accessToken}` } })).status).toBe(405)
  expect((await mcp(env, accessToken, INIT, { "x-norn-actor": "user:someone-else" })).status).toBe(400)
  expect(env.forwarded).toHaveLength(0)
})

test("without STUDIO_MCP the endpoint and the OAuth server do not exist", async () => {
  const env = stage(false)
  expect(await (await call(env, "/mcp", { method: "POST", body: "{}" })).text()).toContain("studio")
  expect(await (await call(env, "/.well-known/oauth-authorization-server")).text()).toContain("studio")
})

test("only the known assistants' callbacks and metadata documents are accepted", async () => {
  for (const uri of [CLAUDE, "https://claude.com/api/mcp/auth_callback", "https://chatgpt.com/connector_platform_oauth_redirect", "https://chatgpt.com/connector/oauth/abc_123", "http://localhost:6274/oauth/callback", "http://127.0.0.1:33418/callback"]) {
    expect(assistantCallback(uri)).toBe(true)
  }
  for (const uri of ["https://evil.example/cb", "https://chatgpt.com/connector/oauth/../x", "https://chatgpt.com.evil.example/connector_platform_oauth_redirect", "http://claude.ai/api/mcp/auth_callback", "https://u:p@claude.ai/api/mcp/auth_callback", "not a url"]) {
    expect(assistantCallback(uri)).toBe(false)
  }
  expect(cimdAllowed("https://chatgpt.com/oauth/client.json")).toBe(true)
  expect(cimdAllowed("https://claude.ai/oauth/claude-client.json")).toBe(true)
  for (const url of ["http://chatgpt.com/oauth/client.json", "https://169.254.169.254/latest", "https://norn-cell.internal/x", "https://chatgpt.com.evil.example/c.json"]) {
    expect(cimdAllowed(url)).toBe(false)
  }
  let fetched = 0
  const counting = (async () => { fetched += 1; return new Response("{}") }) as unknown as typeof fetch
  await expect(fetchClientMetadata("https://10.0.0.1/client.json", undefined, counting)).rejects.toThrow()
  expect(fetched).toBe(0)
  await fetchClientMetadata("https://chatgpt.com/oauth/client.json", undefined, counting)
  expect(fetched).toBe(1)
})

test("the widget pages are served as ui:// resources, to a granted assistant only", async () => {
  const env = stage()
  const page = '<!doctype html><html><head><meta charset="utf-8" /><meta name="norn-widget" content="1" /></head><body>widget …</body></html>'
  // As the deployed static layer answers: `norn.html` redirects to `/widgets/norn`, which serves the page.
  const assets = { fetch: async (request: Request) => {
    const { pathname } = new URL(request.url)
    if (pathname === "/widgets/norn.html") return new Response(null, { status: 307, headers: { location: "/widgets/norn" } })
    return new Response(pathname === "/widgets/norn" ? page : "<!doctype html>studio")
  } } as unknown as Fetcher
  const withWidget = { ...env, ASSETS: assets }
  const { cookie } = await person(withWidget, "wi@example.com")
  const { accessToken } = await grant(withWidget, cookie)
  expect((await mcp(withWidget, null, { jsonrpc: "2.0", id: 1, method: "resources/list" })).status).toBe(401)

  const list = (await (await mcp(withWidget, accessToken, { jsonrpc: "2.0", id: 2, method: "resources/list" })).json()) as { result: { resources: { uri: string; mimeType: string }[] } }
  expect(list.result.resources.map(({ uri }) => uri)).toEqual(["ui://norn/answer.html", "ui://norn/lineage.html", "ui://norn/dashboard.html", "ui://norn/receipt.html", "ui://norn/records.html", "ui://norn/findings.html", "ui://norn/plan.html", "ui://norn/inbox.html"])
  expect(list.result.resources.every(({ mimeType }) => mimeType === "text/html;profile=mcp-app")).toBe(true)

  const read = (await (await mcp(withWidget, accessToken, { jsonrpc: "2.0", id: 3, method: "resources/read", params: { uri: "ui://norn/lineage.html" } })).json()) as { result: { contents: { uri: string; text: string; _meta: { ui: { csp: { connectDomains: string[] } } } }[] } }
  const [content] = read.result.contents
  expect(content?.uri).toBe("ui://norn/lineage.html")
  // The charset declaration stays first; the view and origin follow it.
  expect(content?.text).toStartWith('<!doctype html><html><head><meta charset="utf-8" /><meta name="norn-view" content="lineage">')
  expect(content?.text).toContain(`<meta name="norn-studio" content="${ORIGIN}">`)
  expect(content?._meta.ui.csp.connectDomains).toEqual([])

  const unknown = (await (await mcp(withWidget, accessToken, { jsonrpc: "2.0", id: 4, method: "resources/read", params: { uri: "ui://norn/other.html" } })).json()) as { error: { code: number } }
  expect(unknown.error.code).toBe(-32002)
  // A build without the widget never passes Studio's single-page fallback off as one.
  const none = (await (await mcp(env, accessToken, { jsonrpc: "2.0", id: 5, method: "resources/read", params: { uri: "ui://norn/answer.html" } })).json()) as { error: { code: number } }
  expect(none.error.code).toBe(-32002)
  // A page that does not declare UTF-8 first is not served as a widget.
  const undeclared = { ...env, ASSETS: { fetch: async () => new Response('<!doctype html><html><head><meta name="norn-widget" content="1" /></head><body>w</body></html>') } as unknown as Fetcher }
  const refused = (await (await mcp(undeclared, accessToken, { jsonrpc: "2.0", id: 6, method: "resources/read", params: { uri: "ui://norn/answer.html" } })).json()) as { error: { code: number } }
  expect(refused.error.code).toBe(-32002)
  // Static pages are the edge's; nothing about them reaches the cell.
  expect(env.forwarded).toHaveLength(0)
})

test("a store that cannot be read is a retryable 503, never the person's token being wrong", async () => {
  const env = stage()
  const { cookie } = await person(env, "fi@example.com")
  const { accessToken } = await grant(env, cookie)
  expect((await mcp(env, accessToken, INIT)).status).toBe(200)
  // An assistant drops a connector for the session on 401 invalid_token: a hiccup must not look like one.
  for (const table of ["jwks", "member"]) {
    db(env).run(`alter table "${table}" rename to "${table}_away"`)
    const busy = await mcp(env, accessToken, INIT)
    expect(busy.status).toBe(503)
    expect(busy.headers.get("retry-after")).toBe("2")
    expect(busy.headers.get("www-authenticate")).toBeNull()
    db(env).run(`alter table "${table}_away" rename to "${table}"`)
    expect((await mcp(env, accessToken, INIT)).status).toBe(200)
  }
})

test("a store read that fails once is tried again before it counts as unavailable", async () => {
  let reads = 0
  expect(await retried(async () => { reads += 1; if (reads < 2) throw new Error("opening"); return "keys" }, [1, 1])).toBe("keys")
  expect(reads).toBe(2)
  reads = 0
  await expect(retried(async () => { reads += 1; throw new Error("down") }, [1, 1])).rejects.toThrow("down")
  expect(reads).toBe(3)
})

test("consent binds to the ready workspace /api/me displays even when raw session.activeOrganizationId is null, and rejects mismatched expected org", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "null-active@example.com")
  // Simulate a session row whose raw activeOrganizationId is null: /api/me still resolves the earliest ready workspace.
  db(env).run(`update "session" set "activeOrganizationId" = null where "userId" = ?`, [user])
  const me = (await (await call(env, "/api/me", { headers: { cookie } })).json()) as { organization: { id: string } | null }
  expect(me.organization?.id).toBe(org)

  const clientId = await register(env)
  const { verifier, challenge } = pkce()
  const authorize = await call(env, authorizeUrl(clientId, challenge), { headers: { cookie } })
  expect(authorize.status).toBe(302)
  const consentPage = authorize.headers.get("location") ?? ""
  expect(new URL(consentPage, ORIGIN).pathname).toBe("/oauth/consent")

  // If the consent screen displayed another workspace (switched in another tab), accepting must be refused.
  const oauthQuery = signedQueryOf(consentPage)
  const wrongCtx = await consentContext(env, cookie, oauthQuery)
  const wrongWorkspace = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: "org_different", [CONSENT_APPROVAL_HEADER]: wrongCtx.approvalToken ?? "" },
  )
  expect(wrongWorkspace.status).toBe(409)
  expect(await wrongWorkspace.json()).toMatchObject({ code: "WORKSPACE_CHANGED" })

  // Accepting with the displayed workspace binds consent & access token to `org` (never unscoped).
  const okCtx = await consentContext(env, cookie, oauthQuery)
  const consent = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: okCtx.approvalToken ?? "" },
  )
  expect(consent.status).toBe(200)
  const callback = new URL(((await consent.json()) as { url: string }).url)
  const token = await call(env, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: new URLSearchParams({ grant_type: "authorization_code", code: callback.searchParams.get("code") ?? "", redirect_uri: CLAUDE, client_id: clientId, code_verifier: verifier, resource: RESOURCE }),
  })
  expect(token.status).toBe(200)
  const accessToken = ((await token.json()) as { access_token: string }).access_token
  const claims = JSON.parse(Buffer.from(accessToken.split(".")[1] ?? "", "base64url").toString()) as Record<string, unknown>
  expect(claims[ORG_CLAIM]).toBe(org)
  expect((await mcp(env, accessToken, INIT)).status).toBe(200)
  expect(new URL(env.forwarded.at(-1)?.url ?? "").searchParams.get("orgId")).toBe(org)
})

test("consent refuses without a ready workspace, and /mcp rejects unscoped or revoked-workspace tokens without falling back", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "scoped@example.com")
  // Add a second workspace so any fallback to "first membership" would be caught.
  db(env).run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_second', 'Second', 'second', '{"environmentId":"analytics"}', ?)`, [new Date(Date.now() + 1000).toISOString()])
  db(env).run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_second', 'org_second', ?, 'owner', ?)`, [user, new Date(Date.now() + 1000).toISOString()])

  // Grant scoped to `org`; once membership in `org` is removed, the token must fail 401 even though `org_second` remains.
  const { accessToken } = await grant(env, cookie)
  expect((await mcp(env, accessToken, INIT)).status).toBe(200)
  db(env).run(`delete from "member" where "userId" = ? and "organizationId" = ?`, [user, org])
  expect((await mcp(env, accessToken, INIT)).status).toBe(401)

  // Unscoped tokens (no ORG_CLAIM) are rejected both by grantActor and by /mcp JWT verification.
  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, mcp: true })
  expect(await grantActor(auth, { sub: user })).toBeNull()
  expect(await grantActor(auth, { sub: user, [ORG_CLAIM]: "" })).toBeNull()
  const now = Math.floor(Date.now() / 1000)
  const { token: unscopedJwt } = await auth.api.signJWT({
    body: { payload: { iss: ORIGIN, aud: RESOURCE, azp: "client_test", sub: user, iat: now, exp: now + 300 } },
  })
  expect((await mcp(env, unscopedJwt, INIT)).status).toBe(401)

  // Switch the session to org_second, then mark org_second provisioning -> user has no ready workspace:
  // /authorize immediately refuses 409 NO_ACTIVE_WORKSPACE.
  db(env).run(`update "session" set "activeOrganizationId" = 'org_second' where "userId" = ?`, [user])
  db(env).run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_second'`)
  const clientId = await register(env)
  const blockedAuth = await call(env, authorizeUrl(clientId, pkce().challenge), { headers: { cookie } })
  expect(blockedAuth.status).toBe(409)
  expect(await blockedAuth.json()).toMatchObject({ code: "NO_ACTIVE_WORKSPACE" })

  // If the org turns provisioning between /authorize and /oauth2/consent, accepting is still refused 409 NO_ACTIVE_WORKSPACE,
  // while denying consent still completes cleanly with error=access_denied.
  db(env).run(`update "organization" set "metadata" = '{"environmentId":"analytics"}' where "id" = 'org_second'`)
  const authorize = await call(env, authorizeUrl(clientId, pkce().challenge), { headers: { cookie } })
  expect(authorize.status).toBe(302)
  const consentPage = authorize.headers.get("location") ?? ""
  const oauthQuery = signedQueryOf(consentPage)
  const ctx = await consentContext(env, cookie, oauthQuery)
  db(env).run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_second'`)
  const accepted = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: "org_second", [CONSENT_APPROVAL_HEADER]: ctx.approvalToken ?? "" },
  )
  expect(accepted.status).toBe(409)
  expect(await accepted.json()).toMatchObject({ code: "NO_ACTIVE_WORKSPACE" })
  const denied = await postJson(env, "/api/auth/oauth2/consent", { accept: false, oauth_query: oauthQuery }, { cookie, origin: ORIGIN })
  expect(denied.status).toBe(200)
  expect(new URL(((await denied.json()) as { url: string }).url).searchParams.get("error")).toBe("access_denied")
})

test("consent approval requires single-use approval token, guards persistence at commit, and revokes grants across rejoin", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "pinned-consent@example.com")
  const sqlite = db(env)
  sqlite.run(`update "session" set "activeOrganizationId" = null where "userId" = ?`, [user])

  const clientId = await register(env)
  const { verifier, challenge } = pkce()
  const authorize = await call(env, authorizeUrl(clientId, challenge), { headers: { cookie } })
  expect(authorize.status).toBe(302)
  const oauthQuery = signedQueryOf(authorize.headers.get("location") ?? "")

  // 1. Missing x-norn-expected-org -> 400 WORKSPACE_SCOPE_REQUIRED; missing approval token -> 400 CONSENT_APPROVAL_REQUIRED.
  const missingHeader = await postJson(env, "/api/auth/oauth2/consent", { accept: true, oauth_query: oauthQuery }, { cookie, origin: ORIGIN })
  expect(missingHeader.status).toBe(400)
  expect(await missingHeader.json()).toMatchObject({ code: "WORKSPACE_SCOPE_REQUIRED" })
  const missingApproval = await postJson(env, "/api/auth/oauth2/consent", { accept: true, oauth_query: oauthQuery }, { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org })
  expect(missingApproval.status).toBe(400)
  expect(await missingApproval.json()).toMatchObject({ code: "CONSENT_APPROVAL_REQUIRED" })
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "userId" = ?`).get(user)).toEqual({ n: 0 })

  // 2. Approval token is bound to session + oauth_query and single-use (replay refused 409 CONSENT_APPROVAL_EXPIRED).
  const firstCtx = await consentContext(env, cookie, oauthQuery)
  const wrongQueryRes = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: `${oauthQuery}&tampered=1` },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: firstCtx.approvalToken ?? "" },
  )
  expect(wrongQueryRes.status).toBe(409)
  // The token was consumed on the failed attempt and cannot be replayed even with the real query.
  const replay = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: firstCtx.approvalToken ?? "" },
  )
  expect(replay.status).toBe(409)
  expect(await replay.json()).toMatchObject({ code: "CONSENT_APPROVAL_EXPIRED" })

  // 3. Inject an earlier-created ready membership right before oauthConsent is inserted:
  //    both oauthConsent.referenceId and the issued access token MUST stay pinned to `org`, never drifting to `org_earlier`.
  const pinnedCtx = await consentContext(env, cookie, oauthQuery)
  const origPrepare = sqlite.prepare.bind(sqlite)
  let injected = false
  sqlite.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!injected && sql.toLowerCase().includes(`insert into "oauthconsent"`)) {
      injected = true
      origPrepare(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_earlier', 'Earlier', 'earlier', '{"environmentId":"analytics"}', '2020-01-01T00:00:00.000Z')`).run()
      origPrepare(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_earlier', 'org_earlier', ?, 'owner', '2020-01-01T00:00:00.000Z')`).run(user)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof sqlite.prepare
  const approved = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: pinnedCtx.approvalToken ?? "" },
  )
  sqlite.prepare = origPrepare
  expect(injected).toBe(true)
  expect(approved.status).toBe(200)
  const memberGen1 = (sqlite.query(`select "id" from "member" where "userId" = ? and "organizationId" = ?`).get(user, org) as { id: string }).id
  const expectedRefGen1 = encodeGrantRef(org, memberGen1)
  const storedConsent = sqlite.query(`select "referenceId" from "oauthConsent" where "userId" = ?`).get(user) as { referenceId: string } | null
  expect(storedConsent?.referenceId).toBe(expectedRefGen1)
  const callback = new URL(((await approved.json()) as { url: string }).url)
  const token = await call(env, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: new URLSearchParams({ grant_type: "authorization_code", code: callback.searchParams.get("code") ?? "", redirect_uri: CLAUDE, client_id: clientId, code_verifier: verifier, resource: RESOURCE }),
  })
  expect(token.status).toBe(200)
  const tokenPayload = (await token.json()) as { access_token: string; refresh_token?: string }
  const claims = JSON.parse(Buffer.from(tokenPayload.access_token.split(".")[1] ?? "", "base64url").toString()) as Record<string, unknown>
  expect(claims[ORG_CLAIM]).toBe(org)
  expect(claims[MEMBER_CLAIM]).toBe(memberGen1)

  // 4. Revocation + rejoin into the same org NEVER resurrects pre-revocation access token, refresh token, or consent!
  sqlite.run(`delete from "member" where "userId" = ? and "organizationId" = ?`, [user, org])
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "userId" = ? and "referenceId" = ?`).get(user, expectedRefGen1)).toEqual({ n: 0 })
  sqlite.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_rejoined_gen2', ?, ?, 'owner', ?)`, [org, user, new Date().toISOString()])
  expect((await mcp(env, tokenPayload.access_token, INIT)).status).toBe(401)
  if (tokenPayload.refresh_token) {
    const refreshed = await call(env, "/api/auth/oauth2/token", {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
      body: new URLSearchParams({ grant_type: "refresh_token", refresh_token: tokenPayload.refresh_token, client_id: clientId, resource: RESOURCE }),
    })
    expect(refreshed.status).toBeGreaterThanOrEqual(400)
  }

  // 5. Commit-time SQLite trigger aborts oauthConsent insert on concurrent membership deletion (0 consent rows, 0 codes).
  const clientId2 = await register(env)
  sqlite.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [org, user])
  const auth2 = await call(env, authorizeUrl(clientId2, pkce().challenge), { headers: { cookie } })
  expect(auth2.status).toBe(302)
  const oauthQuery2 = signedQueryOf(auth2.headers.get("location") ?? "")
  const ctx2 = await consentContext(env, cookie, oauthQuery2)
  let revoked = false
  sqlite.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!revoked && sql.toLowerCase().includes(`insert into "oauthconsent"`)) {
      revoked = true
      origPrepare(`delete from "member" where "userId" = ? and "organizationId" = ?`).run(user, org)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof sqlite.prepare
  const failed = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery2 },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctx2.approvalToken ?? "" },
  )
  sqlite.prepare = origPrepare
  expect(revoked).toBe(true)
  expect(failed.status).toBe(409)
  expect(await failed.json()).toMatchObject({ code: "NO_ACTIVE_WORKSPACE" })
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "userId" = ? and "referenceId" like ?`).get(user, `${org}|%`)).toEqual({ n: 0 })
})

test("S3 generation isolation: unredeemed codes, legacy grants, prior consents, and session revocation between consent and code insert", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "gen-isolation@example.com")
  const sqlite = db(env)
  const memberGen1 = (sqlite.query(`select "id" from "member" where "userId" = ? and "organizationId" = ?`).get(user, org) as { id: string }).id
  const clientId = await register(env)

  // 1. Mint an unredeemed authorization code under membership generation 1, then remove & rejoin as generation 2 while session stays live.
  const pkce1 = pkce()
  const auth1 = await call(env, authorizeUrl(clientId, pkce1.challenge), { headers: { cookie } })
  expect(auth1.status).toBe(302)
  const query1 = signedQueryOf(auth1.headers.get("location") ?? "")
  const ctx1 = await consentContext(env, cookie, query1)
  const consent1 = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: query1 },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctx1.approvalToken ?? "" },
  )
  expect(consent1.status).toBe(200)
  const code1 = new URL(((await consent1.json()) as { url: string }).url).searchParams.get("code") ?? ""
  expect(code1.length).toBeGreaterThan(0)

  // Prior consent for the SAME membership generation allows silent re-authorization without prompting.
  const pkceSame = pkce()
  const authSame = await call(env, authorizeUrl(clientId, pkceSame.challenge), { headers: { cookie } })
  expect(authSame.status).toBe(302)
  const sameLoc = new URL(authSame.headers.get("location") ?? "")
  expect(sameLoc.origin + sameLoc.pathname).toBe(CLAUDE)
  expect(sameLoc.searchParams.get("code")).toBeString()

  // Remove memberGen1 and rejoin as memberGen2 while session stays live.
  sqlite.run(`delete from "member" where "id" = ?`, [memberGen1])
  sqlite.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_gen2', ?, ?, 'owner', ?)`, [org, user, new Date().toISOString()])

  // Unredeemed code1 MUST fail exchange (deleted by trigger on member removal, and never inherits m_gen2).
  const exchange1 = await call(env, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: new URLSearchParams({ grant_type: "authorization_code", code: code1, redirect_uri: CLAUDE, client_id: clientId, code_verifier: pkce1.verifier, resource: RESOURCE }),
  })
  expect(exchange1.status).toBeGreaterThanOrEqual(400)

  // Prior consent MUST NOT survive rejoin: /oauth2/authorize redirects back to consent prompt, not directly to callback.
  const authAfterRejoin = await call(env, authorizeUrl(clientId, pkce().challenge), { headers: { cookie } })
  expect(authAfterRejoin.status).toBe(302)
  expect(authAfterRejoin.headers.get("location") ?? "").toContain("/oauth/consent")

  // 2. Legacy bare-org grants (missing `|<memberId>`) are rejected by SQLite triggers on consent, code, refresh, and access token tables.
  expect(() => sqlite.run(
    `insert into "oauthConsent" ("id", "clientId", "userId", "referenceId", "scopes", "createdAt", "updatedAt") values ('legacy_c', ?, ?, ?, '["openid"]', datetime('now'), datetime('now'))`,
    [clientId, user, org],
  )).toThrow("oauth.membership_invalid")
  expect(() => sqlite.run(
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt") values ('legacy_v', 'legacy_code', ?, datetime('now', '+5 minutes'), datetime('now'), datetime('now'))`,
    [JSON.stringify({ type: "authorization_code", userId: user, referenceId: org, sessionId: "none" })],
  )).toThrow("oauth.membership_invalid")

  // 3. Session revoked right before oauthConsent insert -> SQLite trigger aborts (409), 0 consent rows written.
  const clientId3 = await register(env)
  const auth3 = await call(env, authorizeUrl(clientId3, pkce().challenge), { headers: { cookie } })
  const query3 = signedQueryOf(auth3.headers.get("location") ?? "")
  const ctx3 = await consentContext(env, cookie, query3)
  const origPrepare = sqlite.prepare.bind(sqlite)
  let revokedBeforeConsent = false
  sqlite.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!revokedBeforeConsent && sql.toLowerCase().includes(`insert into "oauthconsent"`)) {
      revokedBeforeConsent = true
      origPrepare(`delete from "session" where "userId" = ?`).run(user)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof sqlite.prepare
  const consentSessionRevoked = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: query3 },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctx3.approvalToken ?? "" },
  )
  sqlite.prepare = origPrepare
  expect(revokedBeforeConsent).toBe(true)
  expect(consentSessionRevoked.status).toBe(409)
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "clientId" = ?`).get(clientId3)).toEqual({ n: 0 })

  // 4. Session revoked AFTER oauthConsent insert and BEFORE authorization_code insert ->
  //    code trigger aborts (409) AND newly-inserted oauthConsent row is rolled back (0 consent rows left!).
  const freshSignIn = await postJson(env, "/api/auth/sign-in/email", { email: "gen-isolation@example.com", password: "correct horse battery" }, { origin: ORIGIN })
  const freshCookie = cookieOf(freshSignIn)
  sqlite.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [org, user])
  const clientId4 = await register(env)
  const auth4 = await call(env, authorizeUrl(clientId4, pkce().challenge), { headers: { cookie: freshCookie } })
  const query4 = signedQueryOf(auth4.headers.get("location") ?? "")
  const ctx4 = await consentContext(env, freshCookie, query4)
  let revokedBeforeCode = false
  sqlite.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!revokedBeforeCode && sql.toLowerCase().includes(`insert into "verification"`)) {
      revokedBeforeCode = true
      origPrepare(`delete from "session" where "userId" = ?`).run(user)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof sqlite.prepare
  const codeSessionRevoked = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: query4 },
    { cookie: freshCookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctx4.approvalToken ?? "" },
  )
  sqlite.prepare = origPrepare
  expect(revokedBeforeCode).toBe(true)
  expect(codeSessionRevoked.status).toBe(409)
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "clientId" = ?`).get(clientId4)).toEqual({ n: 0 })

  // 5. R3-1 & R3-2: Per-client approval isolation AND exact rollback of consent UPDATE when code issuance fails.
  const signIn5 = await postJson(env, "/api/auth/sign-in/email", { email: "gen-isolation@example.com", password: "correct horse battery" }, { origin: ORIGIN })
  const cookie5 = cookieOf(signIn5)
  sqlite.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [org, user])
  const clientA = await register(env)
  const clientB = await register(env)
  const memberGen2 = "m_gen2"
  const grantRefGen2 = encodeGrantRef(org, memberGen2)

  // First grant Client A an initial consent (`openid`).
  const authA1Url = `/api/auth/oauth2/authorize?${new URLSearchParams({ response_type: "code", client_id: clientA, redirect_uri: CLAUDE, scope: "openid", state: "st-a1", code_challenge: pkce().challenge, code_challenge_method: "S256", resource: RESOURCE })}`
  const authA1 = await call(env, authA1Url, { headers: { cookie: cookie5 } })
  expect(authA1.status).toBe(302)
  const queryA1 = signedQueryOf(authA1.headers.get("location") ?? "")
  const ctxA1 = await consentContext(env, cookie5, queryA1)
  const okA1 = await postJson(env, "/api/auth/oauth2/consent", { accept: true, oauth_query: queryA1 }, { cookie: cookie5, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctxA1.approvalToken ?? "" })
  expect(okA1.status).toBe(200)
  const initialConsent = sqlite.query(`select "id", "scopes" from "oauthConsent" where "clientId" = ? and "userId" = ? and "referenceId" = ?`).get(clientA, user, grantRefGen2) as { id: string; scopes: string }
  expect(initialConsent).not.toBeNull()

  // While a claimed approval exists ONLY for Client B, inserting/updating Client A's consent directly MUST abort in SQLite!
  const authB = await call(env, authorizeUrl(clientB, pkce().challenge), { headers: { cookie: cookie5 } })
  const queryB = signedQueryOf(authB.headers.get("location") ?? "")
  const ctxB = await consentContext(env, cookie5, queryB)
  expect(ctxB.approvalToken).toBeString()
  const session5 = sqlite.query(`select "id", "token" from "session" where "userId" = ? order by "createdAt" desc limit 1`).get(user) as { id: string; token: string }
  // Insert a claimed approval for Client B in `oauth_consent_claim` to simulate an in-flight Client B consent.
  sqlite.run(
    `insert into "oauth_consent_claim" ("id", "userId", "clientId", "referenceId", "sessionId", "sessionToken", "redirectUri", "codeChallenge", "state", "prevConsent", "expiresAt", "createdAt")
     values ('claim_b', ?, ?, ?, ?, ?, ?, null, null, null, datetime('now', '+5 minutes'), datetime('now'))`,
    [user, clientB, grantRefGen2, session5.id, session5.token, CLAUDE],
  )
  expect(() => sqlite.run(`update "oauthConsent" set "scopes" = '["openid","offline_access"]' where "id" = ?`, [initialConsent.id])).toThrow("oauth.membership_invalid")
  // Clean up simulated claim for Client B before testing Client A update rollback.
  sqlite.run(`delete from "oauth_consent_claim" where "id" = 'claim_b'`)

  // Re-consent Client A with broader scopes (`prompt=consent`, `openid offline_access`) and revoke session right before code insert:
  // the UPDATE to oauthConsent MUST roll back to `initialConsent.scopes` even though the session was revoked!
  const authA2Url = `/api/auth/oauth2/authorize?${new URLSearchParams({ response_type: "code", client_id: clientA, redirect_uri: CLAUDE, scope: "openid offline_access", prompt: "consent", state: "st-a2", code_challenge: pkce().challenge, code_challenge_method: "S256", resource: RESOURCE })}`
  const authA2 = await call(env, authA2Url, { headers: { cookie: cookie5 } })
  expect(authA2.status).toBe(302)
  const queryA2 = signedQueryOf(authA2.headers.get("location") ?? "")
  const ctxA2 = await consentContext(env, cookie5, queryA2)
  let revokedOnUpdate = false
  sqlite.prepare = ((sql: string, ...rest: unknown[]) => {
    if (!revokedOnUpdate && sql.toLowerCase().includes(`insert into "verification"`)) {
      revokedOnUpdate = true
      origPrepare(`delete from "session" where "userId" = ?`).run(user)
    }
    return (origPrepare as (...args: unknown[]) => ReturnType<typeof origPrepare>)(sql, ...rest)
  }) as typeof sqlite.prepare
  const failedUpdate = await postJson(env, "/api/auth/oauth2/consent", { accept: true, oauth_query: queryA2 }, { cookie: cookie5, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctxA2.approvalToken ?? "" })
  sqlite.prepare = origPrepare
  expect(revokedOnUpdate).toBe(true)
  expect(failedUpdate.status).toBe(409)
  const restored = sqlite.query(`select "scopes" from "oauthConsent" where "id" = ?`).get(initialConsent.id) as { scopes: string }
  expect(restored.scopes).toBe(initialConsent.scopes)
  expect(sqlite.query(`select count(*) as n from "oauth_consent_claim"`).get()).toEqual({ n: 0 })

  // 6. R4-1/R4-2 & R5-1/R5-2: Claimed approvals live in `oauth_consent_claim` (immune to Better Auth's
  //    verification expiry cleanup), hide provisional consent from concurrent `/oauth2/authorize` AND
  //    from direct `authorization_code` inserts without `claimId`, AND verify every requested scope/resource
  //    against committed `oauthConsent` even after rollback removes the claim.
  const signIn6 = await postJson(env, "/api/auth/sign-in/email", { email: "gen-isolation@example.com", password: "correct horse battery" }, { origin: ORIGIN })
  const cookie6 = cookieOf(signIn6)
  sqlite.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [org, user])
  const session6 = sqlite.query(`select "id", "token" from "session" where "userId" = ? order by "createdAt" desc limit 1`).get(user) as { id: string; token: string }
  const clientC = await register(env)
  const pkceC = pkce()
  const authC = await call(env, authorizeUrl(clientC, pkceC.challenge), { headers: { cookie: cookie6 } })
  const queryC = signedQueryOf(authC.headers.get("location") ?? "")
  const ctxC = await consentContext(env, cookie6, queryC)
  expect(ctxC.approvalToken).toBeString()
  // Simulate an active claimed approval + provisional consent for Client C.
  sqlite.run(
    `insert into "oauth_consent_claim" ("id", "userId", "clientId", "referenceId", "sessionId", "sessionToken", "redirectUri", "codeChallenge", "state", "prevConsent", "expiresAt", "createdAt")
     values ('claim_c', ?, ?, ?, ?, ?, ?, ?, 'st-1', null, datetime('now', '+5 minutes'), datetime('now'))`,
    [user, clientC, grantRefGen2, session6.id, session6.token, CLAUDE, pkceC.challenge],
  )
  sqlite.run(
    `insert into "oauthConsent" ("id", "clientId", "userId", "referenceId", "scopes", "resources", "createdAt", "updatedAt") values ('prov_c', ?, ?, ?, '["openid","offline_access"]', ?, datetime('now'), datetime('now'))`,
    [clientC, user, grantRefGen2, JSON.stringify([RESOURCE])],
  )
  // Concurrent GET /oauth2/authorize is refused 409 CONSENT_IN_PROGRESS while a claim is active (never reads provisional consent).
  const concurrentAuth = await call(env, authorizeUrl(clientC, pkce().challenge), { headers: { cookie: cookie6 } })
  expect(concurrentAuth.status).toBe(409)
  expect(await concurrentAuth.json()).toMatchObject({ code: "CONSENT_IN_PROGRESS" })
  // Another request with IDENTICAL session/client/grant/redirect/challenge/state but NO claimId (or wrong claimId) MUST abort!
  const stolenCodePayload = {
    type: "authorization_code",
    userId: user,
    sessionId: session6.id,
    referenceId: grantRefGen2,
    resource: [RESOURCE],
    query: { client_id: clientC, redirect_uri: CLAUDE, scope: "openid offline_access", code_challenge: pkceC.challenge, state: "st-1" },
  }
  expect(() => sqlite.run(
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt") values ('stolen_code', 'stolen_1', ?, datetime('now', '+5 minutes'), datetime('now'), datetime('now'))`,
    [JSON.stringify(stolenCodePayload)],
  )).toThrow("oauth.membership_invalid")
  expect(() => sqlite.run(
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt") values ('stolen_code2', 'stolen_2', ?, datetime('now', '+5 minutes'), datetime('now'), datetime('now'))`,
    [JSON.stringify({ ...stolenCodePayload, claimId: "wrong-claim-id" })],
  )).toThrow("oauth.membership_invalid")
  // Claim row is still intact (not consumed by the rejected request).
  expect(sqlite.query(`select count(*) as n from "oauth_consent_claim" where "id" = 'claim_c'`).get()).toEqual({ n: 1 })

  // R5-1: Expire the claimed approval and run Better Auth's verification expiry cleanup (deleting all
  // expired verification rows): `oauth_consent_claim` MUST remain intact and still block both code
  // issuance AND raw concurrent claims until atomically rolled back!
  sqlite.run(`update "oauth_consent_claim" set "expiresAt" = datetime('now', '-5 minutes') where "id" = 'claim_c'`)
  sqlite.run(`delete from "verification" where julianday("expiresAt") < julianday('now')`)
  expect(sqlite.query(`select count(*) as n from "oauth_consent_claim" where "id" = 'claim_c'`).get()).toEqual({ n: 1 })
  expect(() => sqlite.run(
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt") values ('expired_claim_code', 'exp_1', ?, datetime('now', '+5 minutes'), datetime('now'), datetime('now'))`,
    [JSON.stringify(stolenCodePayload)],
  )).toThrow("oauth.membership_invalid")
  expect(() => sqlite.run(
    `insert into "oauth_consent_claim" ("id", "userId", "clientId", "referenceId", "sessionId", "sessionToken", "redirectUri", "codeChallenge", "state", "prevConsent", "expiresAt", "createdAt")
     values ('claim_c_dup', ?, ?, ?, ?, ?, ?, null, null, null, datetime('now', '+5 minutes'), datetime('now'))`,
    [user, clientC, grantRefGen2, session6.id, session6.token, CLAUDE],
  )).toThrow("UNIQUE constraint failed")
  // Submitting the successor consent sweeps the expired claim + its provisional consent atomically and succeeds cleanly!
  const ctxC2 = await consentContext(env, cookie6, queryC)
  expect(ctxC2.approvalToken).toBeString()
  const successor = await postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: queryC },
    { cookie: cookie6, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: ctxC2.approvalToken ?? "" },
  )
  expect(successor.status).toBe(200)
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "id" = 'prov_c'`).get()).toEqual({ n: 0 })
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "clientId" = ? and "userId" = ? and "referenceId" = ?`).get(clientC, user, grantRefGen2)).toEqual({ n: 1 })

  // R5-2: Even after rollback restores prior consent (`openid` only) and deletes the claim, an
  // authorization_code carrying broader provisional scopes (`openid offline_access`) OR an
  // unconsented resource MUST abort in SQLite!
  expect(() => sqlite.run(
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt") values ('stale_scope_code', 'stale_scope_1', ?, datetime('now', '+5 minutes'), datetime('now'), datetime('now'))`,
    [JSON.stringify({
      type: "authorization_code",
      userId: user,
      sessionId: session6.id,
      referenceId: grantRefGen2,
      resource: [RESOURCE],
      query: { client_id: clientA, redirect_uri: CLAUDE, scope: "openid offline_access", code_challenge: pkce().challenge, state: "st-race" },
    })],
  )).toThrow("oauth.membership_invalid")
  expect(() => sqlite.run(
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt") values ('stale_res_code', 'stale_res_1', ?, datetime('now', '+5 minutes'), datetime('now'), datetime('now'))`,
    [JSON.stringify({
      type: "authorization_code",
      userId: user,
      sessionId: session6.id,
      referenceId: grantRefGen2,
      resource: ["https://other.example/mcp"],
      query: { client_id: clientA, redirect_uri: CLAUDE, scope: "openid", code_challenge: pkce().challenge, state: "st-race-res" },
    })],
  )).toThrow("oauth.membership_invalid")
})
