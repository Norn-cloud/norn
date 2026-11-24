import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { billingModeStatement } from "../billing.ts"
import worker, { type Env } from "../index.ts"
import type { WorkspacesDocument } from "../workspaces.ts"

const ORIGIN = "https://studio-alpha.norn.cloud"
const MIGRATIONS = join(import.meta.dir, "../migrations")

function migratedDb(): Database {
  const db = new Database(":memory:")
  for (const file of readdirSync(MIGRATIONS).filter((name) => name.endsWith(".sql")).sort()) {
    db.exec(readFileSync(join(MIGRATIONS, file), "utf8"))
  }
  return db
}

function stage(options: { defaultEnvironmentId?: string; billing?: boolean } = {}) {
  const db = migratedDb()
  db.run(billingModeStatement(options.billing === true))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip",
    ...options.defaultEnvironmentId === undefined ? {} : { STUDIO_DEFAULT_ENVIRONMENT_ID: options.defaultEnvironmentId },
  }
  return { db, env }
}

let ipCounter = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  if (!headers.has("cf-connecting-ip")) headers.set("cf-connecting-ip", `198.51.100.${++ipCounter % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const post = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
  call(env, path, {
    method: "POST",
    body: JSON.stringify(body),
    headers: { origin: ORIGIN, "content-type": "application/json", ...headers },
  })
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")

async function enroll(env: Env, email: string, name = email.split("@")[0] ?? "User") {
  expect((await post(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name })).status).toBe(200)
  const signedIn = await post(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" })
  expect(signedIn.status).toBe(200)
  const db = env.AUTH_DB as unknown as Database
  const { id: userId } = db.query(`select "id" from "user" where "email" = ?`).get(email) as { id: string }
  const { organizationId } = db.query(`select "organizationId" from "member" where "userId" = ? order by "createdAt" asc limit 1`).get(userId) as { organizationId: string }
  return { cookie: cookieOf(signedIn), userId, organizationId }
}

test("GET /api/workspaces requires a session and lists only the caller's workspaces with plan label and cell state", async () => {
  const { db, env } = stage({ defaultEnvironmentId: "prod", billing: true })
  const unauth = await call(env, "/api/workspaces")
  expect(unauth.status).toBe(401)
  expect(unauth.headers.get("cache-control")).toBe("no-store")

  const alice = await enroll(env, "alice@example.com", "Alice")
  const bob = await enroll(env, "bob@example.com", "Bob")
  const carol = await enroll(env, "carol@example.com", "Carol")
  // Add Alice as a viewer to Bob's workspace and to an org with no cell metadata. Carol's org is unrelated.
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_v', ?, ?, 'viewer', ?)`, [bob.organizationId, alice.userId, new Date(Date.now() + 1000).toISOString()])
  db.run(`insert into "organization" ("id", "name", "slug", "metadata", "createdAt") values ('org_nocell', 'Sandbox', 'sandbox', '{}', ?)`, [new Date(Date.now() + 2000).toISOString()])
  db.run(`insert into "member" ("id", "organizationId", "userId", "role", "createdAt") values ('m_nc', 'org_nocell', ?, 'member', ?)`, [alice.userId, new Date(Date.now() + 2000).toISOString()])

  const res = await call(env, "/api/workspaces", { headers: { cookie: alice.cookie } })
  expect(res.status).toBe(200)
  expect(res.headers.get("cache-control")).toBe("no-store")
  const doc = (await res.json()) as WorkspacesDocument
  expect(doc.planLabel).toBe("Free plan")
  expect(doc.workspaces).toHaveLength(3)
  expect(doc.workspaces[0]).toMatchObject({ id: alice.organizationId, name: "Alice's workspace", role: "owner", active: true, hasCell: true })
  expect(doc.workspaces[1]).toMatchObject({ id: bob.organizationId, name: "Bob's workspace", role: "viewer", active: false, hasCell: true })
  expect(doc.workspaces[2]).toMatchObject({ id: "org_nocell", name: "Sandbox", slug: "sandbox", role: "member", active: false, hasCell: false })
  expect(doc.workspaces.some((w) => w.id === carol.organizationId)).toBe(false)

  // Switching through Better Auth's existing set-active route updates which workspace GET /api/workspaces marks active.
  expect((await post(env, "/api/auth/organization/set-active", { organizationId: bob.organizationId }, { cookie: alice.cookie })).status).toBe(200)
  const after = (await (await call(env, "/api/workspaces", { headers: { cookie: alice.cookie } })).json()) as WorkspacesDocument
  expect(after.workspaces.find((w) => w.id === alice.organizationId)?.active).toBe(false)
  expect(after.workspaces.find((w) => w.id === bob.organizationId)?.active).toBe(true)

  // Non-members cannot switch into Carol's workspace.
  expect((await post(env, "/api/auth/organization/set-active", { organizationId: carol.organizationId }, { cookie: alice.cookie })).status).toBe(403)
})

test("GET /api/workspaces omits planLabel when billing is off and enforces allowed HTTP methods", async () => {
  const { env } = stage({ defaultEnvironmentId: "prod", billing: false })
  const alice = await enroll(env, "nobill@example.com", "Alice")
  const doc = (await (await call(env, "/api/workspaces", { headers: { cookie: alice.cookie } })).json()) as WorkspacesDocument
  expect(doc.planLabel).toBeNull()
  expect(doc.workspaces).toHaveLength(1)
  expect(doc.workspaces[0]).toMatchObject({ id: alice.organizationId, name: "Alice's workspace", role: "owner", active: true, hasCell: true })

  const put = await call(env, "/api/workspaces", { method: "PUT", headers: { cookie: alice.cookie, origin: ORIGIN } })
  expect(put.status).toBe(405)
  expect(put.headers.get("allow")).toBe("GET, POST")

  const getAccept = await call(env, "/api/workspaces/invitations/accept", { method: "GET", headers: { cookie: alice.cookie } })
  expect(getAccept.status).toBe(405)
})
