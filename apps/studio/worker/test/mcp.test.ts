import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { createHash, randomBytes } from "node:crypto"
import { assistantCallback, cimdAllowed, CONSENT_APPROVAL_HEADER, createAuth, encodeGrantRef, fetchClientMetadata, MEMBER_CLAIM, ORG_CLAIM } from "../auth.ts"
import type { Env } from "../index.ts"
import { EXPECTED_ORG_HEADER, hostToken } from "../actor.ts"
import { grantActor, retried } from "../mcp.ts"
import { BASE_MIGRATIONS, DEFAULT_PASSWORD, claimsOf, cookieOf, createMigratedDb, createWorkerClient, createWorkerEnv, readMigration as migration, withSqlRace } from "./harness.ts"

const ORIGIN = "https://studio-alpha.norn.test"
const RESOURCE = `${ORIGIN}/mcp`
const CLAUDE = "https://claude.ai/api/mcp/auth_callback"

interface Forwarded { readonly url: string; readonly authorization: string | null; readonly body: string }

function stage(mcp = true): Env & { forwarded: Forwarded[] } {
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
  const { env } = createWorkerEnv({
    migrations: [...BASE_MIGRATIONS, "0007_oauth_provider.sql", "0013_oauth_membership_guards.sql", "0017_oauth_unscoped_grants.sql"],
    origin: ORIGIN,
    overrides: { NORN_HOST: host, ...mcp ? { STUDIO_MCP: "on" } : {} },
  })
  return { ...env, forwarded }
}

const { call, postJson, enroll } = createWorkerClient(ORIGIN, { ipPrefix: "203.0.113", redirect: "manual" })
const db = (env: Env) => env.AUTH_DB as unknown as Database

async function person(env: Env, email: string) {
  const signIn = await enroll(env, email, email)
  expect(signIn.status).toBe(200)
  const user = (db(env).query("select id from user where email = ?").get(email) as { id: string }).id
  const org = (db(env).query("select organizationId from member where userId = ?").get(user) as { organizationId: string }).organizationId
  const metadata = JSON.parse((db(env).query("select metadata from organization where id = ?").get(org) as { metadata: string }).metadata)
  db(env).run("update organization set metadata = ? where id = ?", [JSON.stringify({ ...metadata, environmentId: "analytics" }), org])
  return { cookie: cookieOf(signIn), user, org }
}

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

function authorizeUrl(clientId: string, challenge: string, redirect = CLAUDE, extra: Record<string, string> = {}) {
  return `/api/auth/oauth2/authorize?${new URLSearchParams({ response_type: "code", client_id: clientId, redirect_uri: redirect, scope: "openid offline_access", state: "st-1", code_challenge: challenge, code_challenge_method: "S256", resource: RESOURCE, ...extra })}`
}

const signedQueryOf = (location: string) => new URL(location, ORIGIN).search.slice(1)

async function consentContext(env: Env, cookie: string, oauthQuery: string) {
  const response = await postJson(env, "/api/oauth/consent-context", { oauth_query: oauthQuery }, { cookie, origin: ORIGIN })
  expect(response.status).toBe(200)
  return (await response.json()) as { user: { email: string }; organization: { id: string; name: string } | null; approvalToken: string | null }
}

async function approveConsent(env: Env, cookie: string, oauthQuery: string, org: string, approvalToken: string) {
  return postJson(
    env,
    "/api/auth/oauth2/consent",
    { accept: true, oauth_query: oauthQuery },
    { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org, [CONSENT_APPROVAL_HEADER]: approvalToken },
  )
}

async function beginConsent(env: Env, cookie: string, clientId: string, challenge = pkce().challenge, redirect = CLAUDE, extra?: Record<string, string>) {
  const authorize = await call(env, authorizeUrl(clientId, challenge, redirect, extra), { headers: { cookie } })
  expect(authorize.status).toBe(302)
  const consentPage = authorize.headers.get("location") ?? ""
  expect(new URL(consentPage, ORIGIN).pathname).toBe("/oauth/consent")
  const oauthQuery = signedQueryOf(consentPage)
  const ctx = await consentContext(env, cookie, oauthQuery)
  return { oauthQuery, ctx }
}

async function exchangeCode(env: Env, clientId: string, code: string, verifier: string, redirect = CLAUDE) {
  return call(env, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: new URLSearchParams({ grant_type: "authorization_code", code, redirect_uri: redirect, client_id: clientId, code_verifier: verifier, resource: RESOURCE }),
  })
}

async function grant(env: Env, cookie: string, expectedOrg?: string): Promise<{ accessToken: string; refreshToken?: string; claims: Record<string, unknown> }> {
  const clientId = await register(env)
  const { verifier, challenge } = pkce()
  const { oauthQuery, ctx } = await beginConsent(env, cookie, clientId, challenge)
  expect(ctx.approvalToken).toBeString()
  const consent = await approveConsent(env, cookie, oauthQuery, expectedOrg ?? ctx.organization?.id ?? "", ctx.approvalToken ?? "")
  expect(consent.status).toBe(200)
  const callback = new URL(((await consent.json()) as { url: string }).url)
  expect(`${callback.origin}${callback.pathname}`).toBe(CLAUDE)
  expect(callback.searchParams.get("state")).toBe("st-1")
  expect(callback.searchParams.get("iss")).toBe(ORIGIN)
  const token = await exchangeCode(env, clientId, callback.searchParams.get("code") ?? "", verifier)
  expect(token.status).toBe(200)
  const payload = (await token.json()) as { access_token: string; refresh_token?: string }
  return { accessToken: payload.access_token, refreshToken: payload.refresh_token, claims: claimsOf(payload.access_token) }
}

const insertVerification = (sqlite: Database, id: string, identifier: string, payload: unknown) =>
  sqlite.run(
    `insert into "verification" ("id", "identifier", "value", "expiresAt", "createdAt", "updatedAt") values (?, ?, ?, datetime('now', '+5 minutes'), datetime('now'), datetime('now'))`,
    [id, identifier, JSON.stringify(payload)],
  )

const insertClaim = (sqlite: Database, id: string, userId: string, clientId: string, referenceId: string, session: { id: string; token: string }, codeChallenge: string | null = null, state: string | null = null) =>
  sqlite.run(
    `insert into "oauth_consent_claim" ("id", "userId", "clientId", "referenceId", "sessionId", "sessionToken", "redirectUri", "codeChallenge", "state", "prevConsent", "expiresAt", "createdAt")
     values (?, ?, ?, ?, ?, ?, ?, ?, ?, null, datetime('now', '+5 minutes'), datetime('now'))`,
    [id, userId, clientId, referenceId, session.id, session.token, CLAUDE, codeChallenge, state],
  )

const mcp = (env: Env, token: string | null, message: unknown, headers: HeadersInit = {}) =>
  call(env, "/mcp", { method: "POST", body: JSON.stringify(message), headers: { "content-type": "application/json", accept: "application/json, text/event-stream", ...token === null ? {} : { authorization: `Bearer ${token}` }, ...headers } })
const INIT = { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "test", version: "0" } } }

test("discovery names this origin as the resource's only authorization server", async () => {
  const env = stage()
  const resource = await call(env, "/.well-known/oauth-protected-resource/mcp")
  expect(resource.status).toBe(200)
  expect(await resource.json()).toMatchObject({ resource: RESOURCE, authorization_servers: [ORIGIN] })
  const server = await call(env, "/.well-known/oauth-authorization-server")
  expect(server.status).toBe(200)
  const metadata = (await server.json()) as Record<string, unknown>
  expect(metadata).toMatchObject({ issuer: ORIGIN, client_id_metadata_document_supported: true, authorization_response_iss_parameter_supported: true })
  expect(metadata.code_challenge_methods_supported).toContain("S256")
  expect(metadata.token_endpoint_auth_methods_supported).toContain("none")
  expect(typeof metadata.registration_endpoint).toBe("string")
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
  expect(claimsOf(sent?.authorization ?? "")).toMatchObject({ sub: user, aud: "norn-host", norn: { kind: "user", org, role: "owner", amr: "oauth", accountablePerson: user } })

  expect((await mcp(env, accessToken, { jsonrpc: "2.0", method: "notifications/initialized" })).status).toBe(202)
  db(env).run("delete from member where userId = ?", [user])
  expect((await mcp(env, accessToken, INIT)).status).toBe(401)
})

test("signing in from the assistant's flow resumes it at consent", async () => {
  const env = stage()
  await person(env, "bo@example.com")
  const clientId = await register(env)
  const authorize = await call(env, authorizeUrl(clientId, pkce().challenge))
  expect(authorize.status).toBe(302)
  const signInPage = authorize.headers.get("location") ?? ""
  expect(new URL(signInPage, ORIGIN).pathname).toBe("/oauth/sign-in")
  const signIn = await postJson(env, "/api/auth/sign-in/email", { email: "bo@example.com", password: DEFAULT_PASSWORD, oauth_query: signedQueryOf(signInPage) }, { origin: ORIGIN })
  expect(signIn.status).toBe(200)
  expect(new URL(((await signIn.json()) as { url: string }).url, ORIGIN).pathname).toBe("/oauth/consent")
})

test("a loopback callback registers without naming an application type, and gets a code", async () => {
  const env = stage()
  const { cookie, org } = await person(env, "lo@example.com")
  const loopback = "http://127.0.0.1:33418/callback"
  const clientId = await register(env, loopback)
  const { oauthQuery, ctx } = await beginConsent(env, cookie, clientId, pkce().challenge, loopback)
  const consent = await approveConsent(env, cookie, oauthQuery, org, ctx.approvalToken ?? "")
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
    const location = authorize.headers.get("location") ?? ""
    expect(location).not.toStartWith(evil)
    expect(authorize.status >= 400 || location.includes("error=invalid_redirect")).toBe(true)
  } else {
    expect(response.status).toBe(400)
  }
})

test("tokens for another audience, foreign origins and other methods are refused", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "di@example.com")
  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, mcp: true })
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
  expect(content?.text).toStartWith('<!doctype html><html><head><meta charset="utf-8" /><meta name="norn-view" content="lineage">')
  expect(content?.text).toContain(`<meta name="norn-studio" content="${ORIGIN}">`)
  expect(content?._meta.ui.csp.connectDomains).toEqual([])

  for (const [target, uri] of [
    [withWidget, "ui://norn/other.html"],
    [env, "ui://norn/answer.html"],
    [{ ...env, ASSETS: { fetch: async () => new Response('<!doctype html><html><head><meta name="norn-widget" content="1" /></head><body>w</body></html>') } as unknown as Fetcher }, "ui://norn/answer.html"],
  ] as const) {
    const res = (await (await mcp(target, accessToken, { jsonrpc: "2.0", id: 4, method: "resources/read", params: { uri } })).json()) as { error: { code: number } }
    expect(res.error.code).toBe(-32002)
  }
  expect(env.forwarded).toHaveLength(0)
})

test("a store that cannot be read is a retryable 503, never the person's token being wrong", async () => {
  const env = stage()
  const { cookie } = await person(env, "fi@example.com")
  const { accessToken } = await grant(env, cookie)
  expect((await mcp(env, accessToken, INIT)).status).toBe(200)
  // An assistant drops a connector for the session on 401 invalid_token: a store hiccup must answer 503.
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
  db(env).run(`update "session" set "activeOrganizationId" = null where "userId" = ?`, [user])
  expect(((await (await call(env, "/api/me", { headers: { cookie } })).json()) as { organization: { id: string } | null }).organization?.id).toBe(org)

  const clientId = await register(env)
  const { verifier, challenge } = pkce()
  const { oauthQuery, ctx: wrongCtx } = await beginConsent(env, cookie, clientId, challenge)

  const wrongWorkspace = await approveConsent(env, cookie, oauthQuery, "org_different", wrongCtx.approvalToken ?? "")
  expect(wrongWorkspace.status).toBe(409)
  expect(await wrongWorkspace.json()).toMatchObject({ code: "WORKSPACE_CHANGED" })

  const okCtx = await consentContext(env, cookie, oauthQuery)
  const consent = await approveConsent(env, cookie, oauthQuery, org, okCtx.approvalToken ?? "")
  expect(consent.status).toBe(200)
  const callback = new URL(((await consent.json()) as { url: string }).url)
  const token = await exchangeCode(env, clientId, callback.searchParams.get("code") ?? "", verifier)
  expect(token.status).toBe(200)
  const accessToken = ((await token.json()) as { access_token: string }).access_token
  expect(claimsOf(accessToken)[ORG_CLAIM]).toBe(org)
  expect((await mcp(env, accessToken, INIT)).status).toBe(200)
  expect(new URL(env.forwarded.at(-1)?.url ?? "").searchParams.get("orgId")).toBe(org)
})

test("consent refuses without a ready workspace, and /mcp rejects unscoped or revoked-workspace tokens without falling back", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "scoped@example.com")
  db(env).run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_second', 'Second', 'second', '{"environmentId":"analytics"}', ?)`, [new Date(Date.now() + 1000).toISOString()])
  db(env).run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_second', 'org_second', ?, 'owner', ?)`, [user, new Date(Date.now() + 1000).toISOString()])

  const { accessToken } = await grant(env, cookie)
  expect((await mcp(env, accessToken, INIT)).status).toBe(200)
  db(env).run(`delete from "member" where "userId" = ? and "organizationId" = ?`, [user, org])
  expect((await mcp(env, accessToken, INIT)).status).toBe(401)

  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, mcp: true })
  expect(await grantActor(auth, { sub: user })).toBeNull()
  expect(await grantActor(auth, { sub: user, [ORG_CLAIM]: "" })).toBeNull()
  const now = Math.floor(Date.now() / 1000)
  const { token: unscopedJwt } = await auth.api.signJWT({ body: { payload: { iss: ORIGIN, aud: RESOURCE, azp: "client_test", sub: user, iat: now, exp: now + 300 } } })
  expect((await mcp(env, unscopedJwt, INIT)).status).toBe(401)

  db(env).run(`update "session" set "activeOrganizationId" = 'org_second' where "userId" = ?`, [user])
  db(env).run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_second'`)
  const clientId = await register(env)
  const blockedAuth = await call(env, authorizeUrl(clientId, pkce().challenge), { headers: { cookie } })
  expect(blockedAuth.status).toBe(409)
  expect(await blockedAuth.json()).toMatchObject({ code: "NO_ACTIVE_WORKSPACE" })

  db(env).run(`update "organization" set "metadata" = '{"environmentId":"analytics"}' where "id" = 'org_second'`)
  const { oauthQuery, ctx } = await beginConsent(env, cookie, clientId)
  db(env).run(`update "organization" set "metadata" = '{"norn.provisioning":true}' where "id" = 'org_second'`)
  const accepted = await approveConsent(env, cookie, oauthQuery, "org_second", ctx.approvalToken ?? "")
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

  const missingHeader = await postJson(env, "/api/auth/oauth2/consent", { accept: true, oauth_query: oauthQuery }, { cookie, origin: ORIGIN })
  expect(missingHeader.status).toBe(400)
  expect(await missingHeader.json()).toMatchObject({ code: "WORKSPACE_SCOPE_REQUIRED" })
  const missingApproval = await postJson(env, "/api/auth/oauth2/consent", { accept: true, oauth_query: oauthQuery }, { cookie, origin: ORIGIN, [EXPECTED_ORG_HEADER]: org })
  expect(missingApproval.status).toBe(400)
  expect(await missingApproval.json()).toMatchObject({ code: "CONSENT_APPROVAL_REQUIRED" })
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "userId" = ?`).get(user)).toEqual({ n: 0 })

  const firstCtx = await consentContext(env, cookie, oauthQuery)
  expect((await approveConsent(env, cookie, `${oauthQuery}&tampered=1`, org, firstCtx.approvalToken ?? "")).status).toBe(409)
  const replay = await approveConsent(env, cookie, oauthQuery, org, firstCtx.approvalToken ?? "")
  expect(replay.status).toBe(409)
  expect(await replay.json()).toMatchObject({ code: "CONSENT_APPROVAL_EXPIRED" })

  const pinnedCtx = await consentContext(env, cookie, oauthQuery)
  const { result: approved, raced: injected } = await withSqlRace(
    sqlite,
    `insert into "oauthconsent"`,
    (run) => {
      run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_earlier', 'Earlier', 'earlier', '{"environmentId":"analytics"}', '2020-01-01T00:00:00.000Z')`)
      run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_earlier', 'org_earlier', ?, 'owner', '2020-01-01T00:00:00.000Z')`, user)
    },
    () => approveConsent(env, cookie, oauthQuery, org, pinnedCtx.approvalToken ?? ""),
  )
  expect(injected).toBe(true)
  expect(approved.status).toBe(200)
  const memberGen1 = (sqlite.query(`select "id" from "member" where "userId" = ? and "organizationId" = ?`).get(user, org) as { id: string }).id
  const expectedRefGen1 = encodeGrantRef(org, memberGen1)
  expect((sqlite.query(`select "referenceId" from "oauthConsent" where "userId" = ?`).get(user) as { referenceId: string } | null)?.referenceId).toBe(expectedRefGen1)
  const callback = new URL(((await approved.json()) as { url: string }).url)
  const token = await exchangeCode(env, clientId, callback.searchParams.get("code") ?? "", verifier)
  expect(token.status).toBe(200)
  const tokenPayload = (await token.json()) as { access_token: string; refresh_token?: string }
  expect(claimsOf(tokenPayload.access_token)).toMatchObject({ [ORG_CLAIM]: org, [MEMBER_CLAIM]: memberGen1 })

  sqlite.run(`delete from "member" where "userId" = ? and "organizationId" = ?`, [user, org])
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "userId" = ? and "referenceId" = ?`).get(user, expectedRefGen1)).toEqual({ n: 0 })
  sqlite.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_rejoined_gen2', ?, ?, 'owner', ?)`, [org, user, new Date().toISOString()])
  expect((await mcp(env, tokenPayload.access_token, INIT)).status).toBe(401)
  expect(tokenPayload.refresh_token).toBeTruthy()
  const refreshed = await call(env, "/api/auth/oauth2/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: new URLSearchParams({ grant_type: "refresh_token", refresh_token: tokenPayload.refresh_token!, client_id: clientId, resource: RESOURCE }),
  })
  expect(refreshed.status).toBeGreaterThanOrEqual(400)

  const clientId2 = await register(env)
  sqlite.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [org, user])
  const { oauthQuery: oauthQuery2, ctx: ctx2 } = await beginConsent(env, cookie, clientId2)
  const { result: failed, raced: revoked } = await withSqlRace(
    sqlite,
    `insert into "oauthconsent"`,
    (run) => run(`delete from "member" where "userId" = ? and "organizationId" = ?`, user, org),
    () => approveConsent(env, cookie, oauthQuery2, org, ctx2.approvalToken ?? ""),
  )
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

  const pkce1 = pkce()
  const { oauthQuery: query1, ctx: ctx1 } = await beginConsent(env, cookie, clientId, pkce1.challenge)
  const consent1 = await approveConsent(env, cookie, query1, org, ctx1.approvalToken ?? "")
  expect(consent1.status).toBe(200)
  const code1 = new URL(((await consent1.json()) as { url: string }).url).searchParams.get("code") ?? ""
  expect(code1.length).toBeGreaterThan(0)

  const authSame = await call(env, authorizeUrl(clientId, pkce().challenge), { headers: { cookie } })
  expect(authSame.status).toBe(302)
  const sameLoc = new URL(authSame.headers.get("location") ?? "")
  expect(sameLoc.origin + sameLoc.pathname).toBe(CLAUDE)
  expect(sameLoc.searchParams.get("code")).toBeString()

  sqlite.run(`delete from "member" where "id" = ?`, [memberGen1])
  sqlite.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_gen2', ?, ?, 'owner', ?)`, [org, user, new Date().toISOString()])

  expect((await exchangeCode(env, clientId, code1, pkce1.verifier)).status).toBeGreaterThanOrEqual(400)
  const authAfterRejoin = await call(env, authorizeUrl(clientId, pkce().challenge), { headers: { cookie } })
  expect(authAfterRejoin.status).toBe(302)
  expect(authAfterRejoin.headers.get("location") ?? "").toContain("/oauth/consent")

  expect(() => sqlite.run(
    `insert into "oauthConsent" ("id", "clientId", "userId", "referenceId", "scopes", "createdAt", "updatedAt") values ('legacy_c', ?, ?, ?, '["openid"]', datetime('now'), datetime('now'))`,
    [clientId, user, org],
  )).toThrow("oauth.membership_invalid")
  expect(() => insertVerification(sqlite, "legacy_v", "legacy_code", { type: "authorization_code", userId: user, referenceId: org, sessionId: "none" })).toThrow("oauth.membership_invalid")

  const freshSession = async () => {
    const res = await postJson(env, "/api/auth/sign-in/email", { email: "gen-isolation@example.com", password: DEFAULT_PASSWORD }, { origin: ORIGIN })
    sqlite.run(`update "session" set "activeOrganizationId" = ? where "userId" = ?`, [org, user])
    const row = sqlite.query(`select "id", "token" from "session" where "userId" = ? order by "createdAt" desc limit 1`).get(user) as { id: string; token: string }
    return { cookie: cookieOf(res), session: row }
  }

  for (const triggerSql of [`insert into "oauthconsent"`, `insert into "verification"`]) {
    const activeCookie = triggerSql.includes("oauthconsent") ? cookie : (await freshSession()).cookie
    const cId = await register(env)
    const { oauthQuery, ctx } = await beginConsent(env, activeCookie, cId)
    const { result, raced } = await withSqlRace(
      sqlite,
      triggerSql,
      (run) => run(`delete from "session" where "userId" = ?`, user),
      () => approveConsent(env, activeCookie, oauthQuery, org, ctx.approvalToken ?? ""),
    )
    expect(raced).toBe(true)
    expect(result.status).toBe(409)
    expect(sqlite.query(`select count(*) as n from "oauthConsent" where "clientId" = ?`).get(cId)).toEqual({ n: 0 })
  }

  const { cookie: cookie5, session: session5 } = await freshSession()
  const clientA = await register(env)
  const clientB = await register(env)
  const grantRefGen2 = encodeGrantRef(org, "m_gen2")

  const { oauthQuery: queryA1, ctx: ctxA1 } = await beginConsent(env, cookie5, clientA, pkce().challenge, CLAUDE, { scope: "openid", state: "st-a1" })
  expect((await approveConsent(env, cookie5, queryA1, org, ctxA1.approvalToken ?? "")).status).toBe(200)
  const initialConsent = sqlite.query(`select "id", "scopes" from "oauthConsent" where "clientId" = ? and "userId" = ? and "referenceId" = ?`).get(clientA, user, grantRefGen2) as { id: string; scopes: string }
  expect(initialConsent).not.toBeNull()

  const { ctx: ctxB } = await beginConsent(env, cookie5, clientB)
  expect(ctxB.approvalToken).toBeString()
  insertClaim(sqlite, "claim_b", user, clientB, grantRefGen2, session5)
  expect(() => sqlite.run(`update "oauthConsent" set "scopes" = '["openid","offline_access"]' where "id" = ?`, [initialConsent.id])).toThrow("oauth.membership_invalid")
  sqlite.run(`delete from "oauth_consent_claim" where "id" = 'claim_b'`)

  const { oauthQuery: queryA2, ctx: ctxA2 } = await beginConsent(env, cookie5, clientA, pkce().challenge, CLAUDE, { scope: "openid offline_access", prompt: "consent", state: "st-a2" })
  const { result: failedUpdate, raced: revokedOnUpdate } = await withSqlRace(
    sqlite,
    `insert into "verification"`,
    (run) => run(`delete from "session" where "userId" = ?`, user),
    () => approveConsent(env, cookie5, queryA2, org, ctxA2.approvalToken ?? ""),
  )
  expect(revokedOnUpdate).toBe(true)
  expect(failedUpdate.status).toBe(409)
  expect((sqlite.query(`select "scopes" from "oauthConsent" where "id" = ?`).get(initialConsent.id) as { scopes: string }).scopes).toBe(initialConsent.scopes)
  expect(sqlite.query(`select count(*) as n from "oauth_consent_claim"`).get()).toEqual({ n: 0 })

  const { cookie: cookie6, session: session6 } = await freshSession()
  const clientC = await register(env)
  const pkceC = pkce()
  const { oauthQuery: queryC, ctx: ctxC } = await beginConsent(env, cookie6, clientC, pkceC.challenge)
  expect(ctxC.approvalToken).toBeString()
  insertClaim(sqlite, "claim_c", user, clientC, grantRefGen2, session6, pkceC.challenge, "st-1")
  sqlite.run(
    `insert into "oauthConsent" ("id", "clientId", "userId", "referenceId", "scopes", "resources", "claimId", "createdAt", "updatedAt") values ('prov_c', ?, ?, ?, '["openid","offline_access"]', ?, 'claim_c', datetime('now'), datetime('now'))`,
    [clientC, user, grantRefGen2, JSON.stringify([RESOURCE])],
  )
  expect(() => sqlite.run(`update "oauthConsent" set "scopes" = '["openid"]', "claimId" = 'expired_claim_id' where "id" = 'prov_c'`)).toThrow("oauth.membership_invalid")

  const concurrentAuth = await call(env, authorizeUrl(clientC, pkce().challenge), { headers: { cookie: cookie6 } })
  expect(concurrentAuth.status).toBe(409)
  expect(await concurrentAuth.json()).toMatchObject({ code: "CONSENT_IN_PROGRESS" })

  const stolenCodePayload = {
    type: "authorization_code",
    userId: user,
    sessionId: session6.id,
    referenceId: grantRefGen2,
    resource: [RESOURCE],
    query: { client_id: clientC, redirect_uri: CLAUDE, scope: "openid offline_access", code_challenge: pkceC.challenge, state: "st-1" },
  }
  expect(() => insertVerification(sqlite, "stolen_code", "stolen_1", stolenCodePayload)).toThrow("oauth.membership_invalid")
  expect(() => insertVerification(sqlite, "stolen_code2", "stolen_2", { ...stolenCodePayload, claimId: "wrong-claim-id" })).toThrow("oauth.membership_invalid")
  expect(sqlite.query(`select count(*) as n from "oauth_consent_claim" where "id" = 'claim_c'`).get()).toEqual({ n: 1 })

  sqlite.run(`update "oauth_consent_claim" set "expiresAt" = datetime('now', '-5 minutes') where "id" = 'claim_c'`)
  sqlite.run(`delete from "verification" where julianday("expiresAt") < julianday('now')`)
  expect(sqlite.query(`select count(*) as n from "oauth_consent_claim" where "id" = 'claim_c'`).get()).toEqual({ n: 1 })
  expect(() => insertVerification(sqlite, "expired_claim_code", "exp_1", stolenCodePayload)).toThrow("oauth.membership_invalid")
  expect(() => insertClaim(sqlite, "claim_c_dup", user, clientC, grantRefGen2, session6)).toThrow("UNIQUE constraint failed")

  const ctxC2 = await consentContext(env, cookie6, queryC)
  expect(ctxC2.approvalToken).toBeString()
  expect((await approveConsent(env, cookie6, queryC, org, ctxC2.approvalToken ?? "")).status).toBe(200)
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "id" = 'prov_c'`).get()).toEqual({ n: 0 })
  expect(sqlite.query(`select count(*) as n from "oauthConsent" where "clientId" = ? and "userId" = ? and "referenceId" = ?`).get(clientC, user, grantRefGen2)).toEqual({ n: 1 })

  for (const [id, ident, overrides] of [
    ["stale_scope_code", "stale_scope_1", { query: { client_id: clientA, redirect_uri: CLAUDE, scope: "openid offline_access", code_challenge: pkce().challenge, state: "st-race" } }],
    ["stale_res_code", "stale_res_1", { resource: ["https://other.example/mcp"], query: { client_id: clientA, redirect_uri: CLAUDE, scope: "openid", code_challenge: pkce().challenge, state: "st-race-res" } }],
    ["gone_claim_code", "gone_claim_1", { claimId: "deleted_claim_id", query: { client_id: clientA, redirect_uri: CLAUDE, scope: "openid", code_challenge: pkce().challenge, state: "st-gone-claim" } }],
  ] as const) {
    expect(() => insertVerification(sqlite, id, ident, { ...stolenCodePayload, ...overrides })).toThrow("oauth.membership_invalid")
  }

  // Unsupported optional UserInfo claims in `query.claims` (filtered out by Better Auth before
  // storing `oauthConsent.requestedUserInfoClaims`) do not break approval, while an unconsented
  // supported standard claim (`name`) still aborts in SQLite.
  const clientD = await register(env)
  const { oauthQuery: queryD, ctx: ctxD } = await beginConsent(env, cookie6, clientD, pkce().challenge, CLAUDE, {
    scope: "openid",
    state: "st-claims",
    claims: JSON.stringify({ userinfo: { email: null, custom_unsupported_opt: null } }),
  })
  expect((await approveConsent(env, cookie6, queryD, org, ctxD.approvalToken ?? "")).status).toBe(200)
  expect(() => insertVerification(sqlite, "unconsented_claim_code", "uc_1", {
    ...stolenCodePayload,
    query: { client_id: clientD, redirect_uri: CLAUDE, scope: "openid", claims: { userinfo: { email: null, name: null } }, code_challenge: pkce().challenge, state: "st-uc" },
  })).toThrow("oauth.membership_invalid")
})

test("a session with no active workspace never resolves a grant by falling back to another membership: it must consent again", async () => {
  const env = stage()
  const { cookie, user, org } = await person(env, "no-fallback@example.com")
  await grant(env, cookie, org)
  expect((db(env).query(`select count(*) as n from "oauthConsent" where "userId" = ?`).get(user) as { n: number }).n).toBe(1)
  db(env).run(`update "session" set "activeOrganizationId" = null where "userId" = ?`, [user])
  const clientId = (db(env).query(`select "clientId" from "oauthConsent" where "userId" = ?`).get(user) as { clientId: string }).clientId
  const again = await call(env, authorizeUrl(clientId, pkce().challenge), { headers: { cookie } })
  expect(again.status).toBe(302)
  const location = new URL(again.headers.get("location") ?? "", ORIGIN)
  expect(location.pathname).toBe("/oauth/consent")
  expect(location.searchParams.has("code")).toBe(false)

  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, mcp: true })
  const member = (db(env).query(`select "id" from "member" where "userId" = ?`).get(user) as { id: string }).id
  expect(await grantActor(auth, { sub: user, [ORG_CLAIM]: org, [MEMBER_CLAIM]: member })).toMatchObject({ orgId: org, memberId: member })
  expect(await grantActor(auth, { sub: user, [ORG_CLAIM]: "  ", [MEMBER_CLAIM]: member })).toBeNull()
  expect(await grantActor(auth, { sub: user, [ORG_CLAIM]: "org_elsewhere", [MEMBER_CLAIM]: member })).toBeNull()
})

test("grants recorded before workspace scoping are withdrawn: the person consents again", () => {
  const legacy = createMigratedDb([...BASE_MIGRATIONS, "0007_oauth_provider.sql"])
  const at = new Date().toISOString()
  for (const [id, reference] of [["c_scoped", "org_a|m_a"], ["c_bare", "org_a"], ["c_null", null], ["c_empty_member", "org_a|"]] as const) {
    legacy.run(`insert into "oauthConsent" ("id", "clientId", "userId", "referenceId", "scopes", "createdAt", "updatedAt") values (?, 'client', 'u', ?, '[]', ?, ?)`, [id, reference, at, at])
    legacy.run(`insert into "oauthRefreshToken" ("id", "token", "clientId", "userId", "referenceId", "expiresAt", "createdAt", "scopes") values (?, ?, 'client', 'u', ?, ?, ?, '[]')`, [`r_${id}`, `t_${id}`, reference, at, at])
    legacy.run(`insert into "oauthAccessToken" ("id", "token", "clientId", "userId", "referenceId", "expiresAt", "createdAt", "scopes") values (?, ?, 'client', 'u', ?, ?, ?, '[]')`, [`a_${id}`, `ta_${id}`, reference, at, at])
  }
  legacy.exec(migration("0013_oauth_membership_guards.sql"))
  legacy.exec(migration("0017_oauth_unscoped_grants.sql"))
  expect(legacy.query(`select "id" from "oauthConsent" order by "id"`).all()).toEqual([{ id: "c_scoped" }])
  expect(legacy.query(`select "id" from "oauthRefreshToken" where "revoked" is null`).all()).toEqual([{ id: "r_c_scoped" }])
  expect(legacy.query(`select "id" from "oauthAccessToken" where "revoked" is null`).all()).toEqual([{ id: "a_c_scoped" }])
})
