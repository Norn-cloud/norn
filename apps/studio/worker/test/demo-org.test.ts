import { expect, test } from "bun:test"
import { join } from "node:path"
import { DEMO_ORG, parseMembers, seedDemoOrg, seedStatements, waitForWorker, seedWhenReady, withWrangler, SeedFailure, SEED_EXIT, discoverD1 } from "../../scripts/seed-demo-org.ts"
import { hostToken, resolveActor } from "../actor.ts"
import { createAuth } from "../auth.ts"
import type { Env } from "../index.ts"
import { DEFAULT_ORIGIN as ORIGIN, DEFAULT_PASSWORD, claimsOf, cookieOf, createWorkerClient, createWorkerEnv } from "./harness.ts"

function stage() {
  const { db, env } = createWorkerEnv({ origin: ORIGIN, assetsHtml: "studio", overrides: { STUDIO_DEFAULT_ORG: DEMO_ORG.id } })
  const binding = {
    prepare: (sql: string) => ({ bind: (...params: string[]) => ({ sql, params }) }),
    batch: async (statements: { sql: string; params: string[] }[]) => db.transaction(() => { for (const { sql, params } of statements) db.prepare(sql).run(...params) })(),
  } as unknown as import("../../scripts/seed-demo-org-core.ts").SeedDatabase
  const seed = (members = ["viewer@demo.norn.cloud=viewer", "analyst@demo.norn.cloud=member"]) => seedDemoOrg(binding, parseMembers(members), new Date())
  const verify = (email: string) => db.prepare(`update "user" set "emailVerified" = 1 where "email" = ?`).run(email)
  return { db, env, seed, verify, auth: createAuth({ clientIpHeader: "cf-connecting-ip", database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, defaultOrganizationId: DEMO_ORG.id }) }
}

const { call, post: postJson } = createWorkerClient(ORIGIN)
const post = (env: Env, path: string, body: unknown, cookie?: string) =>
  postJson(env, path, body, cookie === undefined ? {} : { cookie })
const signUp = (env: Env, email: string) => post(env, "/api/auth/sign-up/email", { email, password: DEFAULT_PASSWORD, name: email.split("@")[0] })
const signIn = async (env: Env, email: string) => cookieOf(await post(env, "/api/auth/sign-in/email", { email, password: DEFAULT_PASSWORD }))
const claims = (token: string) => claimsOf<{ norn: Record<string, unknown> }>(token)

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
  await expect(waitForWorker(process.exited, "http://127.0.0.1:1/ready", "test")).rejects.toMatchObject({ exitCode: SEED_EXIT.wranglerExited, message: "Wrangler exited (exit 27)" })
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


test("readiness and seed deadlines report timeout, and refusals report refusal", async () => {
  await expect(waitForWorker(new Promise<number>(() => {}), "http://127.0.0.1:1/ready", "test", 40)).rejects.toMatchObject({ exitCode: SEED_EXIT.timeout })
  let mode: "hang" | "refuse" = "hang"
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch(request) {
    if (new URL(request.url).pathname === "/ready") return new Response(null, { status: 204 })
    return mode === "refuse" ? new Response("not allowed", { status: 403 }) : new Promise<Response>(() => {})
  } })
  try {
    const base = `http://127.0.0.1:${server.port}`
    await expect(seedWhenReady(new Promise<number>(() => {}), base, "test", [], { readiness: 1000, seed: 40 })).rejects.toMatchObject({ exitCode: SEED_EXIT.timeout })
    mode = "refuse"
    await expect(seedWhenReady(new Promise<number>(() => {}), base, "test", [], { readiness: 1000, seed: 1000 })).rejects.toMatchObject({ exitCode: SEED_EXIT.refused })
  } finally { server.stop(true) }
})

test("Wrangler is reaped after success and each failure exit", async () => {
  for (const code of [undefined, SEED_EXIT.timeout, SEED_EXIT.refused, SEED_EXIT.wranglerExited]) {
    const child = Bun.spawn([globalThis.process.execPath, "-e", "setTimeout(() => {}, 60000)"], { stdout: "ignore", stderr: "ignore" })
    const run = withWrangler(child, async () => { if (code !== undefined) throw new SeedFailure("test", code); return "ok" })
    if (code === undefined) expect(await run).toBe("ok")
    else await expect(run).rejects.toMatchObject({ exitCode: code })
    expect(child.exitCode !== null || child.signalCode !== null).toBe(true)
  }
  const alreadyExited = Bun.spawn([globalThis.process.execPath, "-e", "setTimeout(() => {}, 60000)"], { stdout: "ignore", stderr: "ignore" })
  alreadyExited.kill()
  await alreadyExited.exited
  await expect(withWrangler(alreadyExited, async () => { throw new SeedFailure("original", SEED_EXIT.wranglerExited) })).rejects.toThrow("original")
})


test("D1 discovery obeys its deadline", async () => {
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Promise<Response>(() => {}) })
  // The typed client's request, sent to a local server that never answers.
  const stalled = ((url: RequestInfo | URL, init?: RequestInit) => fetch(`http://127.0.0.1:${server.port}${new URL(String(url)).pathname}`, init)) as typeof fetch
  try {
    await expect(discoverD1({ credentials: { accountId: "account", apiToken: "token" }, fetch: stalled }, "norn-studio-auth-preview", 40)).rejects.toMatchObject({ exitCode: SEED_EXIT.timeout, message: "D1 discovery timed out" })
  } finally { server.stop(true) }
})

test("SIGTERM during provisioning reaps Wrangler before the operator exits", async () => {
  const modulePath = join(import.meta.dir, "../../scripts/seed-demo-org.ts")
  const source = `
    import { withWrangler, seedWhenReady } from ${JSON.stringify(modulePath)};
    const worker = Bun.spawn([process.execPath, "-e", "setTimeout(() => {}, 60000)"], { stdout: "ignore", stderr: "ignore" });
    try {
      await withWrangler(worker, async (signal) => {
        console.log(worker.pid);
        await seedWhenReady(worker.exited, "http://127.0.0.1:1", "test", [], undefined, signal);
      });
    } catch (error) { process.exitCode = error.exitCode ?? 1; }
  `
  const operator = Bun.spawn([globalThis.process.execPath, "-e", source], { stdout: "pipe", stderr: "pipe" })
  const reader = operator.stdout.getReader()
  const output = await reader.read()
  const pid = Number(new TextDecoder().decode(output.value).trim())
  expect(Number.isSafeInteger(pid) && pid > 1).toBe(true)
  operator.kill("SIGTERM")
  expect(await operator.exited).toBe(143)
  expect(() => globalThis.process.kill(pid, 0)).toThrow()
})
