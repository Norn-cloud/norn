import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import worker, { type Env } from "../index.ts"
import { GRANTS_PER_WINDOW, GRANT_SECONDS, GRANT_WINDOW_MS, SIDEKICK_CLIENT, grantAllowed } from "../sidekick.ts"

/**
 * Sidekick's grant end to end: a signed-in person mints their own /mcp access, the agent's MCP
 * calls reach the cell as that person with their current role (an analyst stays a member, whose
 * reads the cell masks), and the grant opens nothing but /mcp. The cell is a stub that records
 * what it receives; its masking and reveal refusals are the cell's own tests.
 */

const ORIGIN = "https://studio-alpha.norn.test"
const migration = (name: string) => readFileSync(join(import.meta.dir, "../migrations", name), "utf8")

function stage(mcp = true): Env & { forwarded: { url: string; authorization: string | null }[] } {
  const db = new Database(":memory:")
  for (const name of ["0001_better_auth.sql", "0002_workspace_provisioning.sql", "0007_oauth_provider.sql"]) db.exec(migration(name))
  const forwarded: { url: string; authorization: string | null }[] = []
  const host = {
    fetch: async (request: Request) => {
      forwarded.push({ url: request.url, authorization: request.headers.get("authorization") })
      const message = (await request.json()) as { id?: number }
      return Response.json({ jsonrpc: "2.0", id: message.id, result: { served: true } })
    },
  } as unknown as Fetcher
  const assets = { fetch: async () => new Response("<!doctype html>studio") } as unknown as Fetcher
  return { ASSETS: assets, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "s".repeat(40), BETTER_AUTH_URL: ORIGIN, STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip", NORN_HOST: host, ...mcp ? { STUDIO_MCP: "on" } : {}, forwarded }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { redirect: "manual", ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const postJson = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
  call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", ...headers } })
const db = (env: Env) => env.AUTH_DB as unknown as Database
const claimsOf = (token: string) => JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as Record<string, unknown>

async function signUp(env: Env, email: string) {
  await postJson(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name: email }, { origin: ORIGIN })
  const signIn = await postJson(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" }, { origin: ORIGIN })
  expect(signIn.status).toBe(200)
  const cookie = signIn.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
  const user = (db(env).query("select id from user where email = ?").get(email) as { id: string }).id
  return { cookie, user }
}

/** An owner whose workspace has the `analytics` cell, and an analyst (member) working in it. */
async function workspace(env: Env) {
  const owner = await signUp(env, "owner@example.com")
  const org = (db(env).query("select organizationId from member where userId = ?").get(owner.user) as { organizationId: string }).organizationId
  const metadata = JSON.parse((db(env).query("select metadata from organization where id = ?").get(org) as { metadata: string }).metadata)
  db(env).run("update organization set metadata = ? where id = ?", [JSON.stringify({ ...metadata, environmentId: "analytics" }), org])
  const analyst = await signUp(env, "analyst@example.com")
  db(env).run("insert into member (id, organizationId, userId, role, createdAt) values ('m-analyst', ?, ?, 'member', ?)", [org, analyst.user, new Date().toISOString()])
  db(env).run("update session set activeOrganizationId = ? where userId = ?", [org, analyst.user])
  return { owner, analyst, org }
}

const mint = (env: Env, cookie: string, origin: string | null = ORIGIN) =>
  call(env, "/api/sidekick/grant", { method: "POST", headers: { cookie, ...origin === null ? {} : { origin } } })
const mcp = (env: Env, token: string) =>
  call(env, "/mcp", { method: "POST", body: JSON.stringify({ jsonrpc: "2.0", id: 3, method: "tools/list", params: {} }), headers: { "content-type": "application/json", authorization: `Bearer ${token}` } })

test("a signed-in person's grant is their own /mcp access in their workspace, short-lived, from Studio's own client", async () => {
  const env = stage()
  const { owner, org } = await workspace(env)
  const response = await mint(env, owner.cookie)
  expect(response.status).toBe(200)
  expect(response.headers.get("cache-control")).toBe("no-store")
  const { token, expiresAt } = (await response.json()) as { token: string; expiresAt: number }
  const claims = claimsOf(token)
  expect(claims).toMatchObject({ iss: ORIGIN, aud: `${ORIGIN}/mcp`, sub: owner.user, azp: SIDEKICK_CLIENT, "https://norn.cloud/org": org })
  expect((claims.exp as number) - (claims.iat as number)).toBe(GRANT_SECONDS)
  expect(expiresAt).toBe((claims.exp as number) * 1000)

  expect((await mcp(env, token)).status).toBe(200)
  const sent = env.forwarded.at(-1)
  expect(Object.fromEntries(new URL(sent?.url ?? "").searchParams)).toEqual({ orgId: org, environmentId: "analytics" })
  // The cell never sees the grant, only a host token for the person.
  expect(sent?.authorization).not.toContain(token)
  expect(claimsOf((sent?.authorization ?? "").replace(/^Bearer /, ""))).toMatchObject({ sub: owner.user, aud: "norn-host", norn: { kind: "user", org, role: "owner", amr: "oauth" } })
})

test("an analyst's Sidekick reaches the cell as a member, so the cell masks what they read, and the grant opens no other route", async () => {
  const env = stage()
  const { analyst, org } = await workspace(env)
  const { token } = (await (await mint(env, analyst.cookie)).json()) as { token: string }
  expect((await mcp(env, token)).status).toBe(200)
  expect(claimsOf((env.forwarded.at(-1)?.authorization ?? "").replace(/^Bearer /, ""))).toMatchObject({ sub: analyst.user, norn: { org, role: "member" } })

  // Reveal and every other cell route are behind the session, never the grant.
  const before = env.forwarded.length
  for (const path of ["/api/host/v1/objects/canon.Party/p-1:reveal", "/api/host/v1/defs", "/api/questions/catalog", "/api/me"]) {
    expect((await call(env, path, { method: path.includes(":reveal") ? "POST" : "GET", headers: { authorization: `Bearer ${token}`, origin: ORIGIN } })).status).toBe(401)
  }
  expect(env.forwarded.length).toBe(before)

  // Leaving the workspace ends the grant at once.
  db(env).run("delete from member where id = 'm-analyst'")
  expect((await mcp(env, token)).status).toBe(401)
})

test("no session, another origin, another method, or a stage without /mcp mints nothing", async () => {
  const env = stage()
  const { owner } = await workspace(env)
  expect((await mint(env, "")).status).toBe(401)
  expect((await mint(env, owner.cookie, "https://evil.example")).status).toBe(403)
  expect((await mint(env, owner.cookie, null)).status).toBe(403)
  expect((await call(env, "/api/sidekick/grant", { headers: { cookie: owner.cookie } })).status).toBe(405)
  const off = stage(false)
  const someone = await signUp(off, "someone@example.com")
  expect((await mint(off, someone.cookie)).status).toBe(404)
})

test("a workspace without a cell gets no grant", async () => {
  const env = stage()
  const { cookie } = await signUp(env, "new@example.com")
  expect((await mint(env, cookie)).status).toBe(409)
})

test("a person mints a bounded number of grants per window, then gets 429 until it passes", async () => {
  const store = new Map<string, number[]>()
  for (let i = 0; i < GRANTS_PER_WINDOW; i++) expect(grantAllowed("p", 1_000 + i, store)).toBe(true)
  expect(grantAllowed("p", 2_000, store)).toBe(false)
  expect(grantAllowed("someone else", 2_000, store)).toBe(true)
  expect(grantAllowed("p", 1_000 + GRANT_WINDOW_MS, store)).toBe(true)

  const env = stage()
  const { analyst } = await workspace(env)
  const statuses: number[] = []
  for (let i = 0; i <= GRANTS_PER_WINDOW; i++) statuses.push((await mint(env, analyst.cookie)).status)
  expect(statuses.slice(0, GRANTS_PER_WINDOW).every((status) => status === 200)).toBe(true)
  expect(statuses.at(-1)).toBe(429)
})
