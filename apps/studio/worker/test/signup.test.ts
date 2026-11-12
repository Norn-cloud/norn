import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import worker, { signUpEmails, type Env } from "../index.ts"

const ORIGIN = "https://studio-alpha.norn.cloud"
const migration = (name: string) => readFileSync(join(import.meta.dir, "../migrations", name), "utf8")

function stage(invited?: string) {
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  db.exec(migration("0002_workspace_provisioning.sql"))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    ...invited === undefined ? {} : { STUDIO_SIGNUP_EMAILS: invited },
  }
  return { db, env }
}

let address = 0
const signUp = (env: Env, email: string) =>
  worker.fetch(new Request(`${ORIGIN}/api/auth/sign-up/email`, {
    method: "POST",
    body: JSON.stringify({ email, password: "correct horse battery", name: "someone" }),
    headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": `198.51.100.${++address % 250}` },
  }) as Parameters<typeof worker.fetch>[0], env)
const users = (db: Database) => db.query(`select "email" from "user" order by "email"`).all()

test("an invite-only deployment creates accounts for invited addresses only", async () => {
  const { db, env } = stage(" Owner@Example.com , guest@example.com,")
  // Better Auth answers every sign-up alike (no session either way), so a refused address is
  // told nothing it could not learn otherwise; the account simply does not exist.
  await signUp(env, "stranger@example.com")
  expect(users(db)).toEqual([])
  expect((await signUp(env, "owner@example.com")).ok).toBe(true)
  expect((await signUp(env, "GUEST@example.com")).ok).toBe(true)
  expect(users(db)).toEqual([{ email: "guest@example.com" }, { email: "owner@example.com" }])
})

test("without the setting sign-up stays open, as before", async () => {
  const { db, env } = stage()
  expect((await signUp(env, "anyone@example.com")).ok).toBe(true)
  expect(users(db)).toEqual([{ email: "anyone@example.com" }])
  expect(signUpEmails({})).toBeUndefined()
  expect(signUpEmails({ STUDIO_SIGNUP_EMAILS: "" })).toEqual([])
})
