import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { resolveActor } from "../actor.ts"
import { ATTESTATION_SECONDS, AUTHORITY_AUDIENCE, AUTHORITY_PATH, MEMBERSHIP_METHOD } from "../authority.ts"
import { createAuth } from "../auth.ts"
import worker, { type Env } from "../index.ts"

// APPROVAL-LIVE-1: the cell host looks a requester's current membership up, service to service.
const ORIGIN = "https://studio-preview.norn.cloud"
const CREDENTIAL = "c".repeat(40)

function stage(credential: string | undefined = CREDENTIAL) {
  const db = new Database(":memory:")
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8"))
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0002_workspace_provisioning.sql"), "utf8"))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip",
    ...credential === undefined ? {} : { NORN_AUTHORITY_TOKEN: credential },
  }
  return { env, db, auth: createAuth({ clientIpHeader: "cf-connecting-ip", database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN }) }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const signUp = async (env: Env, email: string) => {
  const init = (body: unknown) => ({ method: "POST", headers: { "content-type": "application/json", origin: ORIGIN }, body: JSON.stringify(body) })
  await call(env, "/api/auth/sign-up/email", init({ email, password: "correct horse battery", name: "Ada" }))
  const response = await call(env, "/api/auth/sign-in/email", init({ email, password: "correct horse battery" }))
  return response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
}
const lookUp = (env: Env, body: unknown, credential = CREDENTIAL) =>
  call(env, AUTHORITY_PATH, { method: "POST", headers: { authorization: `Bearer ${credential}`, "content-type": "application/json" }, body: JSON.stringify(body) })
const claims = (token: string) => JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as Record<string, unknown>

async function member(env: Env, auth: ReturnType<typeof createAuth>, email: string) {
  const cookie = await signUp(env, email)
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  if (actor === null) throw new Error("no actor")
  return actor
}

test("a current member is attested with the role it holds now, for the authority's audience only", async () => {
  const { env, db, auth } = stage()
  const actor = await member(env, auth, "ada@example.com")
  const answered = await lookUp(env, { orgId: actor.orgId, subject: `user:${actor.subjectId}`, kind: "user" })
  expect(answered.status).toBe(200)
  const { token } = (await answered.json()) as { token: string }
  const body = claims(token)
  expect(body).toMatchObject({ iss: ORIGIN, aud: AUTHORITY_AUDIENCE, sub: actor.subjectId })
  expect((body.exp as number) - (body.iat as number)).toBe(ATTESTATION_SECONDS)
  expect(body.norn).toEqual({ kind: "user", org: actor.orgId, role: "owner", amr: MEMBERSHIP_METHOD })
  // A changed role is the role attested next.
  db.run(`update "member" set "role" = 'viewer' where "userId" = ?`, [actor.subjectId])
  const changed = (await (await lookUp(env, { orgId: actor.orgId, subject: `user:${actor.subjectId}`, kind: "user" })).json()) as { token: string }
  expect((claims(changed.token).norn as { role: string }).role).toBe("viewer")
})

test("a removed member, another organization or an unknown subject is no member", async () => {
  const { env, db, auth } = stage()
  const actor = await member(env, auth, "bo@example.com")
  const other = await lookUp(env, { orgId: "org_elsewhere", subject: `user:${actor.subjectId}`, kind: "user" })
  expect(other.status).toBe(404)
  expect(((await other.json()) as { code: string }).code).toBe("authority.not_member")
  db.run(`delete from "member" where "userId" = ?`, [actor.subjectId])
  const removed = await lookUp(env, { orgId: actor.orgId, subject: `user:${actor.subjectId}`, kind: "user" })
  expect(removed.status).toBe(404)
  expect(((await removed.json()) as { code: string }).code).toBe("authority.not_member")
  expect((await lookUp(env, { orgId: actor.orgId, subject: "user:nobody", kind: "user" })).status).toBe(404)
})

test("an API key is a member while it is enabled, unexpired, of the organization and a service role", async () => {
  const { env, db, auth } = stage()
  const actor = await member(env, auth, "cy@example.com")
  const now = new Date().toISOString()
  db.run(`insert into "apikey" ("id", "configId", "referenceId", "key", "enabled", "createdAt", "updatedAt", "metadata") values ('key_1', 'default', ?, 'hashed', 1, ?, ?, ?)`, [actor.orgId, now, now, JSON.stringify({ role: "member" })])
  const service = { orgId: actor.orgId, subject: "service:key_1", kind: "service" }
  const answered = await lookUp(env, service)
  expect(answered.status).toBe(200)
  expect(claims(((await answered.json()) as { token: string }).token).norn).toEqual({ kind: "service", org: actor.orgId, role: "member", amr: MEMBERSHIP_METHOD })
  db.run(`update "apikey" set "enabled" = 0 where "id" = 'key_1'`)
  expect((await lookUp(env, service)).status).toBe(404)
  db.run(`update "apikey" set "enabled" = 1, "expiresAt" = ? where "id" = 'key_1'`, [new Date(Date.now() - 1000).toISOString()])
  expect((await lookUp(env, service)).status).toBe(404)
  db.run(`update "apikey" set "expiresAt" = null, "metadata" = ? where "id" = 'key_1'`, [JSON.stringify({ role: "admin" })])
  expect((await lookUp(env, service)).status).toBe(404)
  // A user subject is never answered as a service, nor the reverse.
  expect((await lookUp(env, { ...service, kind: "user" })).status).toBe(400)
})

test("only the cell host's credential is answered, and only when one is configured", async () => {
  const { env, auth } = stage()
  const actor = await member(env, auth, "di@example.com")
  const lookup = { orgId: actor.orgId, subject: `user:${actor.subjectId}`, kind: "user" }
  expect((await lookUp(env, lookup, "w".repeat(40))).status).toBe(401)
  expect((await call(env, AUTHORITY_PATH, { method: "POST", body: JSON.stringify(lookup) })).status).toBe(401)
  // A session cookie is no credential here.
  expect((await call(env, AUTHORITY_PATH, { method: "GET" })).status).toBe(405)
  const unset = stage(undefined)
  expect((await lookUp(unset.env, lookup)).status).toBe(404)
  const short = stage("short")
  expect((await lookUp(short.env, lookup, "short")).status).toBe(404)
})
