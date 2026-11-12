import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { createHash, randomBytes } from "node:crypto"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { assistantCallback, cimdAllowed, fetchClientMetadata } from "../auth.ts"
import worker, { type Env } from "../index.ts"
import { hostToken } from "../actor.ts"
import { createAuth } from "../auth.ts"

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
  for (const name of ["0001_better_auth.sql", "0002_workspace_provisioning.sql", "0007_oauth_provider.sql"]) db.exec(migration(name))
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
  return { ASSETS: assets, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, NORN_HOST: host, ...mcp ? { STUDIO_MCP: "on" } : {}, forwarded }
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

/** Sign in, consent and exchange the code: the access token an assistant ends up holding. */
async function grant(env: Env, cookie: string): Promise<{ accessToken: string; claims: Record<string, unknown> }> {
  const clientId = await register(env)
  const { verifier, challenge } = pkce()
  const authorize = await call(env, authorizeUrl(clientId, challenge), { headers: { cookie } })
  expect(authorize.status).toBe(302)
  const consentPage = authorize.headers.get("location") ?? ""
  expect(new URL(consentPage, ORIGIN).pathname).toBe("/oauth/consent")
  const consent = await postJson(env, "/api/auth/oauth2/consent", { accept: true, oauth_query: signedQueryOf(consentPage) }, { cookie, origin: ORIGIN })
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
  const accessToken = ((await token.json()) as { access_token: string }).access_token
  return { accessToken, claims: JSON.parse(Buffer.from(accessToken.split(".")[1] ?? "", "base64url").toString()) }
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
  const auth = createAuth({ database: env.AUTH_DB, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, mcp: true })
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
