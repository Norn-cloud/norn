import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { HOST_TOKEN_SECONDS, hostToken, resolveActor } from "../actor.ts"
import { createAuth } from "../auth.ts"
import worker, { type Env } from "../index.ts"

const ORIGIN = "https://studio-preview.norn.cloud"
const MIGRATION: string = readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8")

function stage(host?: (request: Request) => Response) {
  const db = new Database(":memory:")
  db.exec(MIGRATION)
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0002_workspace_provisioning.sql"), "utf8"))
  const seen: Request[] = []
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    ...host === undefined ? {} : { NORN_HOST: { fetch: async (request: Request) => { seen.push(request); return host(request) } } as unknown as Fetcher },
  }
  return { env, db, seen, auth: createAuth({ database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN }) }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
/** Sign up (no session is issued), then sign in with the password; returns the session cookie. */
const signUp = async (env: Env, email: string) => {
  const init = (body: unknown) => ({ method: "POST", headers: { "content-type": "application/json", origin: ORIGIN }, body: JSON.stringify(body) })
  await call(env, "/api/auth/sign-up/email", init({ email, password: "correct horse battery", name: "Ada" }))
  const response = await call(env, "/api/auth/sign-in/email", init({ email, password: "correct horse battery" }))
  return response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
}
const claims = (token: string) => JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as Record<string, unknown>

test("a password session resolves to a user actor in its organization", async () => {
  const { env, auth } = stage()
  const cookie = await signUp(env, "ada@example.com")
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  expect(actor).toMatchObject({ actorKind: "user", role: "owner", authMethod: "password" })
  expect(actor?.orgId).toBeString()
  expect(await resolveActor(new Request(ORIGIN), auth)).toBeNull()
})

test("the host token carries exactly the actor, for the host's audience, for five minutes", async () => {
  const { env, auth } = stage()
  const cookie = await signUp(env, "bo@example.com")
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  if (actor === null) throw new Error("no actor")
  const body = claims(await hostToken(auth, actor, 1_800_000_000))
  expect(body).toMatchObject({ iss: ORIGIN, aud: "norn-host", sub: actor.subjectId, iat: 1_800_000_000, exp: 1_800_000_000 + HOST_TOKEN_SECONDS })
  expect(body.norn).toEqual({ kind: "user", org: actor.orgId, role: "owner", amr: "password" })
})

test("an organization API key acts as a service, never with an approving role", async () => {
  const { env, auth } = stage()
  const cookie = await signUp(env, "cy@example.com")
  const owner = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  const create = (metadata?: Record<string, string>) => auth.api.createApiKey({ body: { name: "svc", organizationId: owner?.orgId, ...metadata === undefined ? {} : { metadata } }, headers: new Headers({ cookie }) })
  const viewer = await create()
  const service = await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": viewer.key } }), auth)
  expect(service).toEqual({ actorKind: "service", subjectId: viewer.id, orgId: owner?.orgId ?? "", role: "viewer", authMethod: "api_key" })
  const admin = await create({ role: "admin" })
  expect(await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": admin.key } }), auth)).toBeNull()
  expect(await resolveActor(new Request(ORIGIN, { headers: { "x-api-key": "not-a-key" } }), auth)).toBeNull()
})

test("Norn calls go to the host with the host token only, never the client's cookie or key", async () => {
  const { env, seen } = stage(() => Response.json({ ok: true }))
  const cookie = await signUp(env, "di@example.com")
  const response = await call(env, "/api/host/v1/defs?orgId=o", { headers: { cookie, authorization: "Bearer forged", accept: "application/json" } })
  expect(response.status).toBe(200)
  const forwarded = seen[0]
  expect(forwarded?.url).toBe("https://norn-host/v1/defs?orgId=o")
  expect(forwarded?.headers.get("cookie")).toBeNull()
  expect(forwarded?.headers.get("authorization")).not.toBe("Bearer forged")
  expect(claims(forwarded?.headers.get("authorization")?.slice("Bearer ".length) ?? "").aud).toBe("norn-host")
})

test("without a session or a connected host, Norn calls refuse", async () => {
  expect((await call(stage(() => new Response()).env, "/api/host/v1/defs")).status).toBe(401)
  const { env } = stage()
  const cookie = await signUp(env, "ed@example.com")
  expect((await call(env, "/api/host/v1/defs", { headers: { cookie } })).status).toBe(503)
})

test("a request body reaches the host unchanged", async () => {
  const { env, seen } = stage(() => new Response(null, { status: 202 }))
  const cookie = await signUp(env, "fe@example.com")
  const response = await call(env, "/api/host/v1/actions", { method: "POST", headers: { cookie, origin: ORIGIN, "content-type": "application/json" }, body: JSON.stringify({ proposal: 1 }) })
  expect(response.status).toBe(202)
  expect((await seen[0]?.json()) as unknown).toEqual({ proposal: 1 })
  expect(seen[0]?.headers.get("content-type")).toBe("application/json")
})

test("a cookie-authenticated write from anywhere but the Studio origin never reaches the host", async () => {
  const { env, seen } = stage(() => new Response(null, { status: 202 }))
  const cookie = await signUp(env, "gu@example.com")
  for (const origin of ["https://evil.norn.cloud", "null", undefined]) {
    const headers: Record<string, string> = { cookie, "content-type": "text/plain" }
    if (origin !== undefined) headers.origin = origin
    const response = await call(env, "/api/host/v1/actions", { method: "POST", headers, body: "{}" })
    expect([origin, response.status]).toEqual([origin, 403])
  }
  expect(seen).toHaveLength(0)
  // A read needs no Origin; a write from the Studio itself goes through.
  expect((await call(env, "/api/host/v1/defs", { headers: { cookie } })).status).toBe(202)
  expect((await call(env, "/api/host/v1/actions", { method: "POST", headers: { cookie, origin: ORIGIN }, body: "{}" })).status).toBe(202)
  expect(seen).toHaveLength(2)
})

test("an API-key write needs no Origin", async () => {
  const { env, seen, auth } = stage(() => new Response(null, { status: 202 }))
  const cookie = await signUp(env, "ha@example.com")
  const owner = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  const key = await auth.api.createApiKey({ body: { name: "svc", organizationId: owner?.orgId }, headers: new Headers({ cookie }) })
  const response = await call(env, "/api/host/v1/actions", { method: "POST", headers: { "x-api-key": key.key, "content-type": "application/json" }, body: "{}" })
  expect(response.status).toBe(202)
  expect(seen).toHaveLength(1)
})

test("a member removed from the organization, or switched to one they left, reaches nothing there", async () => {
  const { env, db, seen, auth } = stage(() => new Response(null, { status: 202 }))
  const ownerCookie = await signUp(env, "io@example.com")
  const owner = await resolveActor(new Request(ORIGIN, { headers: { cookie: ownerCookie } }), auth)
  const memberCookie = await signUp(env, "ja@example.com")
  const memberId = (db.query("select id from user where email = 'ja@example.com'").get() as { id: string }).id
  db.run("insert into member (id, organizationId, userId, role, createdAt) values ('m-ja', ?, ?, 'member', ?)", [owner?.orgId ?? "", memberId, new Date().toISOString()])
  db.run("update session set activeOrganizationId = ? where userId = ?", [owner?.orgId ?? "", memberId])
  const asMember = () => resolveActor(new Request(ORIGIN, { headers: { cookie: memberCookie } }), auth)
  expect((await asMember())?.orgId).toBe(owner?.orgId)
  db.run("delete from member where id = 'm-ja'")
  expect(await asMember()).toBeNull()
  expect((await call(env, "/api/host/v1/defs", { headers: { cookie: memberCookie } })).status).toBe(401)
  const setActive = await call(env, "/api/auth/organization/set-active", { method: "POST", headers: { cookie: memberCookie, origin: ORIGIN, "content-type": "application/json" }, body: JSON.stringify({ organizationId: owner?.orgId }) })
  expect(setActive.status).toBe(403)
  expect(seen).toHaveLength(0)
})
