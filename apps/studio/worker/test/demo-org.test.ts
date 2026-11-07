import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { DEMO_ORG, parseMembers, seedDemoOrg, seedStatements, waitForWorker, seedWhenReady } from "../../scripts/seed-demo-org.ts"
import { hostToken, resolveActor } from "../actor.ts"
import { createAuth } from "../auth.ts"
import worker, { type Env } from "../index.ts"

const ORIGIN = "https://studio-preview.norn.cloud"
const MIGRATION: string = readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8")

function stage() {
  const db = new Database(":memory:")
  db.exec(MIGRATION)
  db.exec(readFileSync(join(import.meta.dir, "../migrations/0002_workspace_provisioning.sql"), "utf8"))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    STUDIO_DEFAULT_ORG: DEMO_ORG.id,
  }
  const binding = {
    prepare: (sql: string) => ({ bind: (...params: string[]) => ({ sql, params }) }),
    batch: async (statements: { sql: string; params: string[] }[]) => db.transaction(() => { for (const { sql, params } of statements) db.prepare(sql).run(...params) })(),
  } as unknown as import("../../scripts/seed-demo-org-core.ts").SeedDatabase
  const seed = (members = ["viewer@demo.norn.cloud=viewer", "analyst@demo.norn.cloud=member"]) => seedDemoOrg(binding, parseMembers(members), new Date())
  const verify = (email: string) => db.prepare(`update "user" set "emailVerified" = 1 where "email" = ?`).run(email)
  return { db, env, seed, verify, auth: createAuth({ database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, defaultOrganizationId: DEMO_ORG.id }) }
}

let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const post = (env: Env, path: string, body: unknown, cookie?: string) =>
  call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin: ORIGIN, ...cookie === undefined ? {} : { cookie } } })
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
const signUp = (env: Env, email: string) => post(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name: email.split("@")[0] })
const signIn = async (env: Env, email: string) => cookieOf(await post(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" }))
const claims = (token: string) => JSON.parse(Buffer.from(token.split(".")[1] ?? "", "base64url").toString()) as { norn: Record<string, unknown> }

test("seeding twice makes one Norn demo organization with each demo member once, in its role", async () => {
  const { db, env, seed, verify } = stage()
  for (const email of ["viewer@demo.norn.cloud", "analyst@demo.norn.cloud"]) await signUp(env, email)
  verify("viewer@demo.norn.cloud")
  verify("analyst@demo.norn.cloud")
  await seed()
  await seed()
  expect(db.query(`select "name", "slug" from "organization" where "id" = ?`).all(DEMO_ORG.id)).toEqual([{ name: "Norn demo", slug: "norn-demo" }])
  expect(db.query(`select "user"."email" as email, "role" from "member" join "user" on "user"."id" = "member"."userId" where "organizationId" = ? order by email`).all(DEMO_ORG.id))
    .toEqual([{ email: "analyst@demo.norn.cloud", role: "member" }, { email: "viewer@demo.norn.cloud", role: "viewer" }])
})

test("a demo member's session starts in org_demo, and the host token it mints carries norn.org = org_demo", async () => {
  const { env, seed, verify, auth } = stage()
  await signUp(env, "viewer@demo.norn.cloud")
  verify("viewer@demo.norn.cloud")
  await seed(["viewer@demo.norn.cloud=viewer"])
  const cookie = await signIn(env, "viewer@demo.norn.cloud")
  const me = await (await call(env, "/api/me", { headers: { cookie } })).json() as { organization: { id: string; role: string } }
  expect(me.organization).toMatchObject({ id: DEMO_ORG.id, role: "viewer" })
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie } }), auth)
  if (actor === null) throw new Error("no actor")
  expect(claims(await hostToken(auth, actor)).norn).toMatchObject({ org: DEMO_ORG.id, role: "viewer", kind: "user" })
})

test("after switching into org_demo the token names it; someone who is not a member cannot switch in", async () => {
  const { env, seed, verify, auth } = stage()
  await signUp(env, "analyst@demo.norn.cloud")
  await signUp(env, "outsider@example.com")
  verify("analyst@demo.norn.cloud")
  await seed(["analyst@demo.norn.cloud=member"])
  const analyst = await signIn(env, "analyst@demo.norn.cloud")
  expect((await post(env, "/api/auth/organization/set-active", { organizationId: DEMO_ORG.id }, analyst)).status).toBe(200)
  const actor = await resolveActor(new Request(ORIGIN, { headers: { cookie: analyst } }), auth)
  if (actor === null) throw new Error("no actor")
  expect(claims(await hostToken(auth, actor)).norn).toMatchObject({ org: DEMO_ORG.id, role: "member" })
  const outsider = await signIn(env, "outsider@example.com")
  expect((await post(env, "/api/auth/organization/set-active", { organizationId: DEMO_ORG.id }, outsider)).status).toBe(403)
  const me = await (await call(env, "/api/me", { headers: { cookie: outsider } })).json() as { organization: { id: string } }
  expect(me.organization.id).not.toBe(DEMO_ORG.id)
})

test("without a default organization a session starts in the earliest membership, as before", async () => {
  const { db, seed, verify } = stage()
  const env: Env = { ...stage().env, AUTH_DB: db as unknown as D1Database, STUDIO_DEFAULT_ORG: undefined }
  await signUp(env, "viewer@demo.norn.cloud")
  verify("viewer@demo.norn.cloud")
  await seed(["viewer@demo.norn.cloud=viewer"])
  const me = await (await call(env, "/api/me", { headers: { cookie: await signIn(env, "viewer@demo.norn.cloud") } })).json() as { organization: { id: string } }
  expect(me.organization.id).not.toBe(DEMO_ORG.id)
})

test("the seed refuses an owner role or a malformed member", () => {
  expect(() => parseMembers(["a@b.c=owner"])).toThrow("admin, member or viewer")
  expect(() => parseMembers(["nobody=viewer"])).toThrow("not an email")
  expect(() => parseMembers(["a@b.c=viewer=admin"])).toThrow("email=role")
})


test("provisioning refuses an unverified pre-registered account, including an existing membership", async () => {
  const { db, env, seed, verify } = stage()
  const email = "analyst@demo.norn.cloud"
  await signUp(env, email)
  await expect(seed([`${email}=admin`])).rejects.toThrow("NOT NULL constraint failed: member.id")
  expect(db.query(`select * from "member" where "organizationId" = ?`).all(DEMO_ORG.id)).toEqual([])
  verify(email)
  await seed([`${email}=viewer`])
  db.prepare(`update "user" set "emailVerified" = 0 where "email" = ?`).run(email)
  await expect(seed([`${email}=admin`])).rejects.toThrow("NOT NULL constraint failed: member.id")
  expect(db.query(`select "role" from "member" where "organizationId" = ?`).get(DEMO_ORG.id)).toEqual({ role: "viewer" })
  // Both membership writes remain guarded for an unverified account.
  const writes = seedStatements(parseMembers([`${email}=admin`]), new Date())
  for (const statement of writes.slice(1, 3)) db.prepare(statement.sql).run(...statement.params)
  expect(db.query(`select "role" from "member" where "organizationId" = ?`).get(DEMO_ORG.id)).toEqual({ role: "viewer" })
})

test("a later membership write failure rolls back all earlier grants", async () => {
  const { db, env, seed, verify } = stage()
  for (const email of ["analyst@demo.norn.cloud", "viewer@demo.norn.cloud"]) { await signUp(env, email); verify(email) }
  db.exec(`create trigger fail_viewer before insert on "member" when new."userId" = (select "id" from "user" where "email" = 'viewer@demo.norn.cloud') begin select raise(abort, 'injected later failure'); end`)
  await expect(seed(["analyst@demo.norn.cloud=member", "viewer@demo.norn.cloud=viewer"])).rejects.toThrow("injected later failure")
  expect(db.query(`select * from "member" where "organizationId" = ?`).all(DEMO_ORG.id)).toEqual([])
  expect(db.query(`select * from "organization" where "id" = ?`).all(DEMO_ORG.id)).toEqual([])
})


test("provisioning fails promptly when Wrangler exits", async () => {
  const process = Bun.spawn([globalThis.process.execPath, "-e", "process.exit(27)"], { stdout: "ignore", stderr: "ignore" })
  await expect(waitForWorker(process.exited, "http://127.0.0.1:1/ready", "test")).rejects.toThrow("exit 27")
})


test("readiness retries never seed and the seed request is sent once", async () => {
  const calls: string[] = []
  let attempts = 0
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch(request) {
    const path = new URL(request.url).pathname
    calls.push(`${request.method} ${path}`)
    if (path === "/ready") return new Response(null, { status: ++attempts === 1 ? 503 : 204 })
    return new Response("ok")
  } })
  try {
    const response = await seedWhenReady(new Promise<number>(() => {}), `http://127.0.0.1:${server.port}`, "test", [])
    expect(response.status).toBe(200)
    expect(calls).toEqual(["GET /ready", "GET /ready", "POST /seed"])
  } finally { server.stop(true) }
})
