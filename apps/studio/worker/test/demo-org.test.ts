import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { DEMO_ORG, parseMembers, seedStatements } from "../../scripts/seed-demo-org.ts"
import { hostToken, resolveActor } from "../actor.ts"
import { createAuth } from "../auth.ts"
import worker, { type Env } from "../index.ts"

/** The Worker's execution context; these requests schedule no background work. */
const CONTEXT = { waitUntil: () => {}, passThroughOnException: () => {}, props: {} } as unknown as ExecutionContext
const ORIGIN = "https://studio-preview.norn.cloud"
const MIGRATION: string = readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8")

function stage() {
  const db = new Database(":memory:")
  db.exec(MIGRATION)
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    STUDIO_DEFAULT_ORG: DEMO_ORG.id,
  }
  const seed = () => { for (const { sql, params } of seedStatements(parseMembers(["viewer@demo.norn.cloud=viewer", "analyst@demo.norn.cloud=admin"]), new Date())) db.prepare(sql).run(...params) }
  return { db, env, seed, auth: createAuth({ database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, defaultOrganizationId: DEMO_ORG.id }) }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env, CONTEXT)
}
const post = (env: Env, path: string, body: unknown, cookie?: string) =>
  call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin: ORIGIN, ...cookie === undefined ? {} : { cookie } } })
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
const signUp = (env: Env, email: string) => post(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name: email.split("@")[0] })
const signIn = async (env: Env, email: string) => cookieOf(await post(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" }))
const claims = (token: string) => JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as { norn: Record<string, unknown> }

test("seeding twice makes one Norn demo organization with each demo member once, in its role", async () => {
  const { db, env, seed } = stage()
  for (const email of ["viewer@demo.norn.cloud", "analyst@demo.norn.cloud"]) await signUp(env, email)
  seed()
  seed()
  expect(db.query(`select json_extract(metadata, '$.environmentId') as environmentId from organization where id = ?`).get(DEMO_ORG.id)).toEqual({ environmentId: "analytics" })
  expect(db.query(`select "name", "slug" from "organization" where "id" = ?`).all(DEMO_ORG.id)).toEqual([{ name: "Norn demo", slug: "norn-demo" }])
  expect(db.query(`select "user"."email" as email, "role" from "member" join "user" on "user"."id" = "member"."userId" where "organizationId" = ? order by email`).all(DEMO_ORG.id))
    .toEqual([{ email: "analyst@demo.norn.cloud", role: "admin" }, { email: "viewer@demo.norn.cloud", role: "viewer" }])
})

test("a demo member's session starts in org_demo, and the host token it mints carries norn.org = org_demo", async () => {
  const { env, seed, auth } = stage()
  await signUp(env, "viewer@demo.norn.cloud")
  seed()
  const cookie = await signIn(env, "viewer@demo.norn.cloud")
  const me = await (await call(env, "/api/me", { headers: { cookie } })).json() as { organization: { id: string; role: string } }
  expect(me.organization).toMatchObject({ id: DEMO_ORG.id, role: "viewer" })
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  if (actor === null) throw new Error("no actor")
  expect(claims(await hostToken(auth, actor)).norn).toMatchObject({ org: DEMO_ORG.id, role: "viewer", kind: "user" })
})

test("after switching into org_demo the token names it; someone who is not a member cannot switch in", async () => {
  const { env, seed, auth } = stage()
  await signUp(env, "analyst@demo.norn.cloud")
  await signUp(env, "outsider@example.com")
  seed()
  const analyst = await signIn(env, "analyst@demo.norn.cloud")
  expect((await post(env, "/api/auth/organization/set-active", { organizationId: DEMO_ORG.id }, analyst)).status).toBe(200)
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie: analyst } }), auth)
  if (actor === null) throw new Error("no actor")
  expect(claims(await hostToken(auth, actor)).norn).toMatchObject({ org: DEMO_ORG.id, role: "admin" })
  const outsider = await signIn(env, "outsider@example.com")
  expect((await post(env, "/api/auth/organization/set-active", { organizationId: DEMO_ORG.id }, outsider)).status).toBe(403)
  const me = await (await call(env, "/api/me", { headers: { cookie: outsider } })).json() as { organization: { id: string } }
  expect(me.organization.id).not.toBe(DEMO_ORG.id)
})

test("without a default organization a session starts in the earliest membership, as before", async () => {
  const { db, seed } = stage()
  const env: Env = { ...stage().env, AUTH_DB: db as unknown as D1Database, STUDIO_DEFAULT_ORG: undefined }
  await signUp(env, "viewer@demo.norn.cloud")
  seed()
  const me = await (await call(env, "/api/me", { headers: { cookie: await signIn(env, "viewer@demo.norn.cloud") } })).json() as { organization: { id: string } }
  expect(me.organization.id).not.toBe(DEMO_ORG.id)
})

test("the seed refuses an owner role or a malformed member", () => {
  expect(() => parseMembers(["a@b.c=owner"])).toThrow("admin, member or viewer")
  expect(() => parseMembers(["nobody=viewer"])).toThrow("not an email")
})
