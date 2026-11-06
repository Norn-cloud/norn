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
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")

test("sign-up sets an HTTP-only, secure, same-site session cookie and an owned organization", async () => {
  const env = stage()
  const signUp = await post(env, "/api/auth/sign-up/email", { email: "ada@example.com", password: "correct horse battery", name: "Ada" })
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
  const signUp = await post(env, "/api/auth/sign-up/email", { email: "bo@example.com", password: "correct horse battery", name: "Bo" })
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
