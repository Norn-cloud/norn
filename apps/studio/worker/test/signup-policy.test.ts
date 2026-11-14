import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import worker, { type Env } from "../index.ts"

// Migration 0009: a self-hosted install's first account owns it; after that, sign-up is by invitation.

const ORIGIN = "https://studio-preview.norn.cloud"
const migration = (name: string) => readFileSync(join(import.meta.dir, "../migrations", name), "utf8")

function stage(policy?: string): { env: Env; db: Database } {
  const db = new Database(":memory:")
  for (const name of ["0001_better_auth.sql", "0002_workspace_provisioning.sql", "0009_signup_policy.sql"]) db.exec(migration(name))
  if (policy !== undefined) db.query(`insert into "studio_setting" ("key", "value") values ('signup', ?)`).run(policy)
  const assets = { fetch: async () => new Response("<!doctype html>studio") } as unknown as Fetcher
  return { db, env: { ASSETS: assets, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip" } }
}

let address = 0
const post = (env: Env, path: string, body: unknown, cookie?: string) => {
  const headers = new Headers({ "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": `198.51.100.${++address % 250}` })
  if (cookie !== undefined) headers.set("cookie", cookie)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { method: "POST", body: JSON.stringify(body), headers }) as Parameters<typeof worker.fetch>[0], env)
}
const signUp = (env: Env, email: string) => post(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name: email.split("@")[0] })
const signIn = async (env: Env, email: string) =>
  (await post(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" })).headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
const users = (db: Database) => (db.query(`select count(*) as n from "user"`).get() as { n: number }).n

test("with no policy row, sign-up stays open as before", async () => {
  const { env, db } = stage()
  for (const email of ["ada@example.com", "bo@example.com"]) expect((await signUp(env, email)).status).toBe(200)
  expect(users(db)).toBe(2)
})

test("first-owner: the first account signs up and owns its workspace; a second address is refused and nothing is written", async () => {
  const { env, db } = stage("first-owner")
  expect((await signUp(env, "owner@example.com")).status).toBe(200)
  const owner = db.query(`select "member"."role" from "member" join "user" on "user"."id" = "member"."userId" where "user"."email" = 'owner@example.com'`).get() as { role: string }
  expect(owner.role).toBe("owner")
  const refused = await signUp(env, "stranger@example.com")
  expect(refused.status).toBe(403)
  expect(((await refused.json()) as { error: string }).error).toContain("by invitation")
  expect(users(db)).toBe(1)
})

test("first-owner: an invited address signs up, in any letter case; an expired or revoked invitation does not admit", async () => {
  const { env, db } = stage("first-owner")
  expect((await signUp(env, "owner@example.com")).status).toBe(200)
  const cookie = await signIn(env, "owner@example.com")
  // A real invitation through Better Auth, so the stored date encoding is the one the policy reads.
  const invited = await post(env, "/api/auth/organization/invite-member", { email: "Guest@Example.com", role: "member" }, cookie)
  expect(invited.status).toBe(200)
  expect((await signUp(env, "guest@example.com")).status).toBe(200)
  // Expired and canceled invitations admit nobody.
  const { organizationId, inviterId } = db.query(`select "organizationId", "inviterId" from "invitation" limit 1`).get() as { organizationId: string; inviterId: string }
  const add = (email: string, status: string, expiresAt: string) =>
    db.query(`insert into "invitation" ("id", "organizationId", "email", "role", "status", "expiresAt", "createdAt", "inviterId") values (?, ?, ?, 'member', ?, ?, ?, ?)`)
      .run(crypto.randomUUID(), organizationId, email, status, expiresAt, new Date().toISOString(), inviterId)
  add("late@example.com", "pending", new Date(Date.now() - 60_000).toISOString())
  add("gone@example.com", "canceled", new Date(Date.now() + 86_400_000).toISOString())
  for (const email of ["late@example.com", "gone@example.com"]) expect((await signUp(env, email)).status).toBe(403)
  expect(users(db)).toBe(2)
})

test("the database decides, not the Worker's check: an insert past the policy aborts", () => {
  const { db } = stage("first-owner")
  const insert = db.query(`insert into "user" ("id", "name", "email", "emailVerified", "createdAt", "updatedAt") values (?, 'x', ?, 0, ?, ?)`)
  const at = new Date().toISOString()
  insert.run("u1", "first@example.com", at, at)
  expect(() => insert.run("u2", "second@example.com", at, at)).toThrow("signup.invite_only")
  expect(users(db)).toBe(1)
  // 'open' lifts it, and no other value can be stored.
  db.query(`update "studio_setting" set "value" = 'open' where "key" = 'signup'`).run()
  insert.run("u2", "second@example.com", at, at)
  expect(() => db.query(`update "studio_setting" set "value" = 'anyone' where "key" = 'signup'`).run()).toThrow()
})
