import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { hostToken, resolveActor } from "../actor.ts"
import { ATTESTATION_SECONDS, AUTHORITY_AUDIENCE, AUTHORITY_PATH, MEMBERSHIP_METHOD } from "../authority.ts"
import { createAuth } from "../auth.ts"
import worker, { type Env } from "../index.ts"

// APPROVAL-LIVE-1: the cell host looks a requester's current membership up, service to service.
const ORIGIN = "https://studio-preview.norn.cloud"
const CREDENTIAL = "c".repeat(40)

// `null`: no credential configured (an explicit `undefined` would take the default).
function stage(credential: string | null = CREDENTIAL) {
  const db = new Database(":memory:")
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8"))
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0002_workspace_provisioning.sql"), "utf8"))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip",
    ...credential === null ? {} : { NORN_AUTHORITY_TOKEN: credential },
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
// The cell's one-use challenge and the attempt it bound the lookup to.
const CHALLENGE = "a".repeat(64)
const BINDING = "norn.approval/1:dec_1:cmd_1"
const claims = (token: string) => JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as Record<string, unknown>

async function member(env: Env, auth: ReturnType<typeof createAuth>, email: string) {
  const cookie = await signUp(env, email)
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  if (actor === null) throw new Error("no actor")
  return actor
}

type Actor = Awaited<ReturnType<typeof member>>

/** A lookup about `subject` in `actor`'s organization, on behalf of `actor`'s live request. */
async function asked(auth: ReturnType<typeof createAuth>, actor: Actor, subject: string, kind = "user", extra: Record<string, unknown> = {}) {
  return { orgId: actor.orgId, subject, kind, challenge: CHALLENGE, binding: BINDING, onBehalfOf: await hostToken(auth, actor), ...extra }
}

test("a current member is attested with the role it holds now, for its challenge and attempt, for the authority's audience only", async () => {
  const { env, db, auth } = stage()
  const actor = await member(env, auth, "ada@example.com")
  const answered = await lookUp(env, await asked(auth, actor, `user:${actor.subjectId}`))
  expect(answered.status).toBe(200)
  const { token } = (await answered.json()) as { token: string }
  const body = claims(token)
  expect(body).toMatchObject({ iss: ORIGIN, aud: AUTHORITY_AUDIENCE, sub: actor.subjectId })
  expect((body.exp as number) - (body.iat as number)).toBe(ATTESTATION_SECONDS)
  expect(body.norn).toEqual({ kind: "user", org: actor.orgId, amr: MEMBERSHIP_METHOD, member: true, role: "owner", nonce: CHALLENGE, bind: BINDING })
  // A changed role is the role attested next.
  db.run(`update "member" set "role" = 'viewer' where "userId" = ?`, [actor.subjectId])
  const changed = (await (await lookUp(env, await asked(auth, actor, `user:${actor.subjectId}`))).json()) as { token: string }
  expect((claims(changed.token).norn as { role: string }).role).toBe("viewer")
})

test("a removed member or an unknown subject gets a signed negative for the same lookup (R1-04)", async () => {
  const { env, db, auth } = stage()
  const actor = await member(env, auth, "bo@example.com")
  // The removed member's own (still valid) host token is the live request here.
  const onBehalf = { ...actor }
  db.run(`delete from "member" where "userId" = ?`, [actor.subjectId])
  const removed = await lookUp(env, await asked(auth, onBehalf, `user:${actor.subjectId}`))
  expect(removed.status).toBe(200)
  const negative = claims(((await removed.json()) as { token: string }).token)
  expect(negative).toMatchObject({ aud: AUTHORITY_AUDIENCE, sub: actor.subjectId })
  expect(negative.norn).toEqual({ kind: "user", org: actor.orgId, amr: MEMBERSHIP_METHOD, member: false, nonce: CHALLENGE, bind: BINDING })
  const unknown = await lookUp(env, await asked(auth, onBehalf, "user:nobody"))
  expect((claims(((await unknown.json()) as { token: string }).token).norn as { member: boolean }).member).toBe(false)
})

test("an API key is a member while it is enabled, unexpired, of the organization and a service role", async () => {
  const { env, db, auth } = stage()
  const actor = await member(env, auth, "cy@example.com")
  const now = new Date().toISOString()
  db.run(`insert into "apikey" ("id", "configId", "referenceId", "key", "enabled", "createdAt", "updatedAt", "metadata") values ('key_1', 'default', ?, 'hashed', 1, ?, ?, ?)`, [actor.orgId, now, now, JSON.stringify({ role: "member" })])
  const service = await asked(auth, actor, "service:key_1", "service")
  const memberOf = async () => {
    const response = await lookUp(env, service)
    expect(response.status).toBe(200)
    return claims(((await response.json()) as { token: string }).token).norn as Record<string, unknown>
  }
  expect(await memberOf()).toEqual({ kind: "service", org: actor.orgId, amr: MEMBERSHIP_METHOD, member: true, role: "member", nonce: CHALLENGE, bind: BINDING })
  db.run(`update "apikey" set "enabled" = 0 where "id" = 'key_1'`)
  expect((await memberOf()).member).toBe(false)
  db.run(`update "apikey" set "enabled" = 1, "expiresAt" = ? where "id" = 'key_1'`, [new Date(Date.now() - 1000).toISOString()])
  expect((await memberOf()).member).toBe(false)
  db.run(`update "apikey" set "expiresAt" = null, "metadata" = ? where "id" = 'key_1'`, [JSON.stringify({ role: "admin" })])
  expect((await memberOf()).member).toBe(false)
  // A user subject is never answered as a service, nor the reverse.
  expect((await lookUp(env, { ...service, kind: "user" })).status).toBe(400)
})

test("a lookup answers only for the organization of a live request the cell host serves (R1-08)", async () => {
  const { env, auth } = stage()
  const actor = await member(env, auth, "eve@example.com")
  const other = await member(env, auth, "fay@example.com")
  const subject = `user:${actor.subjectId}`
  // The credential alone queries nothing.
  const bare = await asked(auth, actor, subject)
  delete (bare as Record<string, unknown>).onBehalfOf
  expect((await lookUp(env, bare)).status).toBe(403)
  // Another organization's request is no scope for this one.
  const foreign = await lookUp(env, await asked(auth, actor, subject, "user", { onBehalfOf: await hostToken(auth, other) }))
  expect(foreign.status).toBe(403)
  expect(((await foreign.json()) as { code: string }).code).toBe("authority.out_of_scope")
  // An attestation, or an expired or forged token, is no live request.
  const attested = ((await (await lookUp(env, await asked(auth, actor, subject))).json()) as { token: string }).token
  expect((await lookUp(env, await asked(auth, actor, subject, "user", { onBehalfOf: attested }))).status).toBe(403)
  const expired = await hostToken(auth, actor, Math.floor(Date.now() / 1000) - 3600)
  expect((await lookUp(env, await asked(auth, actor, subject, "user", { onBehalfOf: expired }))).status).toBe(403)
  const live = await hostToken(auth, actor)
  const forged = `${live.slice(0, live.lastIndexOf(".") + 1)}${"A".repeat(86)}`
  expect((await lookUp(env, await asked(auth, actor, subject, "user", { onBehalfOf: forged }))).status).toBe(403)
  // A challenge or binding that is not the cell's form is refused.
  expect((await lookUp(env, await asked(auth, actor, subject, "user", { challenge: "short" }))).status).toBe(400)
  expect((await lookUp(env, await asked(auth, actor, subject, "user", { binding: "anything" }))).status).toBe(400)
})

test("only the cell host's credential is answered, and only when one is configured", async () => {
  const { env, auth } = stage()
  const actor = await member(env, auth, "di@example.com")
  const lookup = await asked(auth, actor, `user:${actor.subjectId}`)
  expect((await lookUp(env, lookup, "w".repeat(40))).status).toBe(401)
  expect((await call(env, AUTHORITY_PATH, { method: "POST", body: JSON.stringify(lookup) })).status).toBe(401)
  // A session cookie is no credential here.
  expect((await call(env, AUTHORITY_PATH, { method: "GET" })).status).toBe(405)
  const unset = stage(null)
  expect((await lookUp(unset.env, lookup)).status).toBe(404)
  const short = stage("short")
  expect((await lookUp(short.env, lookup, "short")).status).toBe(404)
})
