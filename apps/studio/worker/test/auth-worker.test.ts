import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import worker, { type Env } from "../index.ts"

const ORIGIN = "https://studio-preview.norn.cloud"
const MIGRATION: string = readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8")

function stage(): Env {
  const db = new Database(":memory:")
  db.exec(MIGRATION)
  const assets = { fetch: async () => new Response("<!doctype html>studio") } as unknown as Fetcher
  return { ASSETS: assets, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  if (!headers.has("cf-connecting-ip")) headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const post = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
  call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin: ORIGIN, ...headers } })
/** Sign up (no session is issued), then sign in with the password; the sign-in response carries the cookie. */
const enroll = async (env: Env, email: string, name: string, headers: HeadersInit = {}) => {
  const signUp = await post(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name }, headers)
  if (signUp.status !== 200) return signUp
  return post(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" }, headers)
}
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")

test("signing in after sign-up sets an HTTP-only, secure, same-site session cookie and an owned organization", async () => {
  const env = stage()
  const signUp = await enroll(env, "ada@example.com", "Ada")
  expect(signUp.status).toBe(200)
  const session = signUp.headers.getSetCookie().find((line) => line.startsWith("__Secure-norn.session_token="))
  expect(session).toBeDefined()
  expect(session).toMatch(/HttpOnly/i)
  expect(session).toMatch(/Secure/i)
  expect(session).toMatch(/SameSite=Lax/i)
  const me = await call(env, "/api/me", { headers: { cookie: cookieOf(signUp) } })
  expect(me.status).toBe(200)
  const body = (await me.json()) as { user: { email: string }; organization: { role: string; name: string } }
  expect(body.user.email).toBe("ada@example.com")
  expect(body.organization).toMatchObject({ role: "owner", name: "Ada's workspace" })
  const row = (env.AUTH_DB as unknown as Database).query("select authMethod from session").get() as { authMethod: string }
  expect(row.authMethod).toBe("password")
})

test("sign-out ends the session", async () => {
  const env = stage()
  const signUp = await enroll(env, "bo@example.com", "Bo")
  const cookie = cookieOf(signUp)
  expect((await post(env, "/api/auth/sign-out", {}, { cookie })).status).toBe(200)
  expect((await call(env, "/api/me", { headers: { cookie } })).status).toBe(401)
})

test("a state-changing auth request from another origin is refused", async () => {
  const env = stage()
  const response = await post(env, "/api/auth/sign-up/email", { email: "eve@example.com", password: "correct horse battery", name: "Eve" }, { origin: "https://evil.example" })
  expect(response.status).toBe(403)
})

test("repeated password guesses from one address are rate limited", async () => {
  const env = stage()
  await post(env, "/api/auth/sign-up/email", { email: "cy@example.com", password: "correct horse battery", name: "Cy" })
  const statuses: number[] = []
  for (let attempt = 0; attempt < 12; attempt++) {
    statuses.push((await post(env, "/api/auth/sign-in/email", { email: "cy@example.com", password: "wrong password!" }, { "cf-connecting-ip": "203.0.113.7" })).status)
  }
  expect(statuses.slice(0, 10).every((status) => status === 401)).toBe(true)
  expect(statuses.at(-1)).toBe(429)
})

test("magic links refuse when the deployment has no mail sender", async () => {
  const env = stage()
  const response = await post(env, "/api/auth/sign-in/magic-link", { email: "di@example.com" })
  expect(response.status).toBe(503)
})

test("a client-supplied actor header is refused and other APIs need a session", async () => {
  const env = stage()
  expect((await call(env, "/api/me", { headers: { "x-norn-actor": "admin" } })).status).toBe(400)
  expect((await call(env, "/api/anything")).status).toBe(401)
  expect(await (await call(env, "/")).text()).toContain("studio")
})

test("sign-in always lands in the earliest of several organizations", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  const signUp = await enroll(env, "fa@example.com", "Fa")
  const own = (db.query("select organizationId from member").get() as { organizationId: string }).organizationId
  const user = (db.query("select id from user").get() as { id: string }).id
  db.run("insert into organization (id, name, slug, createdAt) values ('later', 'Later', 'later', ?)", [new Date(Date.now() + 60_000).toISOString()])
  db.run("insert into member (id, organizationId, userId, role, createdAt) values ('m2', 'later', ?, 'viewer', ?)", [user, new Date(Date.now() + 60_000).toISOString()])
  await post(env, "/api/auth/sign-out", {}, { cookie: cookieOf(signUp) })
  for (let attempt = 0; attempt < 3; attempt++) {
    const signIn = await post(env, "/api/auth/sign-in/email", { email: "fa@example.com", password: "correct horse battery" })
    const me = (await (await call(env, "/api/me", { headers: { cookie: cookieOf(signIn) } })).json()) as { organization: { id: string; role: string } }
    expect(me.organization).toMatchObject({ id: own, role: "owner" })
  }
})

test("an unverified account cannot accept an invitation sent to its address", async () => {
  const env = stage()
  const owner = cookieOf(await enroll(env, "ow@example.com", "Ow"))
  const org = ((await (await call(env, "/api/me", { headers: { cookie: owner } })).json()) as { organization: { id: string } }).organization.id
  const invite = await post(env, "/api/auth/organization/invite-member", { email: "claim@example.com", role: "admin", organizationId: org }, { cookie: owner })
  expect(invite.status).toBe(200)
  const { id } = (await invite.json()) as { id: string }
  const claimant = cookieOf(await enroll(env, "claim@example.com", "Claim"))
  const accept = await post(env, "/api/auth/organization/accept-invitation", { invitationId: id }, { cookie: claimant })
  expect(accept.status).toBe(403)
  const db = env.AUTH_DB as unknown as Database
  expect(db.query("select count(*) as n from member where organizationId = ?").get(org)).toEqual({ n: 1 })
})

test("sign-up answers a registered address byte for byte like a new one, apart from a placeholder id", async () => {
  const env = stage()
  await post(env, "/api/auth/sign-up/email", { email: "gi@example.com", password: "correct horse battery", name: "Gi" })
  const answer = async (email: string) => {
    const response = await post(env, "/api/auth/sign-up/email", { email, password: "another horse battery", name: "Gi" })
    return { status: response.status, cookies: response.headers.getSetCookie(), text: await response.text() }
  }
  const existing = await answer("gi@example.com")
  const fresh = await answer("ha@example.com")
  const masked = (text: string, email: string) => text.replace(/"id":"[0-9a-f-]{36}"/, '"id":"ID"').replace(email, "EMAIL")
  expect(masked(existing.text, "gi@example.com")).toBe(masked(fresh.text, "ha@example.com"))
  expect(masked(fresh.text, "ha@example.com")).toBe('{"token":null,"user":{"id":"ID","email":"EMAIL","name":"Gi","emailVerified":false}}')
  expect([existing.status, fresh.status, existing.cookies, fresh.cookies]).toEqual([200, 200, [], []])
  // The registered path's matching writes leave nothing behind.
  const db = env.AUTH_DB as unknown as Database
  expect(db.query("select count(*) as n from verification where identifier like 'signup-pad:%'").get()).toEqual({ n: 0 })
  // The registered account keeps its original password, and the new one can sign in.
  expect((await post(env, "/api/auth/sign-in/email", { email: "gi@example.com", password: "another horse battery" })).status).toBe(401)
  expect((await post(env, "/api/auth/sign-in/email", { email: "gi@example.com", password: "correct horse battery" })).status).toBe(200)
  expect((await post(env, "/api/auth/sign-in/email", { email: "ha@example.com", password: "another horse battery" })).status).toBe(200)
})


test("a failed membership insert is retried safely on the next sign-in", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  expect((await enroll(env, "recover@example.com", "Recover")).status).toBe(200)
  db.run(`delete from "member"`)
  db.run(`delete from "organization"`)
  db.exec(`create trigger fail_member before insert on "member" begin select raise(abort, 'injected membership failure'); end`)
  const failed = await post(env, "/api/auth/sign-in/email", { email: "recover@example.com", password: "correct horse battery" })
  expect(failed.status).not.toBe(200)
  expect(db.query(`select count(*) as n from "organization"`).get()).toEqual({ n: 1 })
  expect(db.query(`select count(*) as n from "member"`).get()).toEqual({ n: 0 })
  db.exec(`drop trigger fail_member`)
  const recovered = await post(env, "/api/auth/sign-in/email", { email: "recover@example.com", password: "correct horse battery" })
  expect(recovered.status).toBe(200)
  const me = await (await call(env, "/api/me", { headers: { cookie: cookieOf(recovered) } })).json() as { organization: { role: string; name: string } }
  expect(me.organization).toMatchObject({ role: "owner", name: "Recover's workspace" })
  expect(db.query(`select count(*) as n from "organization"`).get()).toEqual({ n: 1 })
})

test("recovery refuses a workspace whose server ownership proof is altered", async () => {
  const env = stage()
  const db = env.AUTH_DB as unknown as Database
  expect((await enroll(env, "proof@example.com", "Proof")).status).toBe(200)
  db.run(`delete from "member"`)
  db.run(`update "organization" set "metadata" = ?`, ['{"studioProvisioningProof":"forged"}'])
  const response = await post(env, "/api/auth/sign-in/email", { email: "proof@example.com", password: "correct horse battery" })
  expect(response.status).not.toBe(200)
  expect(db.query(`select count(*) as n from "member"`).get()).toEqual({ n: 0 })
})
