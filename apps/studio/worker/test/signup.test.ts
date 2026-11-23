import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { INVITE_ONLY_MESSAGE } from "../auth.ts"
import worker, { magicLinkEnabled, signUpEmails, type Env } from "../index.ts"

const ORIGIN = "https://studio-alpha.norn.cloud"
const migration = (name: string) => readFileSync(join(import.meta.dir, "../migrations", name), "utf8")

function stage(options: { invited?: string; magicLink?: boolean; emailFrom?: string } = {}) {
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  db.exec(migration("0002_workspace_provisioning.sql"))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
    STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip",
    ...options.invited === undefined ? {} : { STUDIO_SIGNUP_EMAILS: options.invited },
    ...options.magicLink ? { EMAIL: { send: async () => undefined }, EMAIL_FROM: options.emailFrom ?? "studio@norn.cloud" } : {},
    ...!options.magicLink && options.emailFrom !== undefined ? { EMAIL_FROM: options.emailFrom } : {},
  }
  return { db, env }
}

let address = 0
const signUp = (env: Env, email: string, options: { path?: string; ip?: string; origin?: string } = {}) =>
  worker.fetch(new Request(`${ORIGIN}${options.path ?? "/api/auth/sign-up/email"}`, {
    method: "POST",
    body: JSON.stringify({ email, password: "correct horse battery", name: "someone" }),
    headers: {
      "content-type": "application/json",
      origin: options.origin ?? ORIGIN,
      "cf-connecting-ip": options.ip ?? `198.51.100.${++address % 250}`,
    },
  }) as Parameters<typeof worker.fetch>[0], env)

const users = (db: Database) => db.query(`select "email" from "user" order by "email"`).all()
const count = (db: Database, table: string) => (db.query(`select count(*) as n from "${table}"`).get() as { n: number }).n

test("an invite-only deployment explicitly refuses non-allowlisted sign-up without creating any account", async () => {
  const { db, env } = stage({ invited: " Owner@Example.com , guest@example.com," })
  const refused = await signUp(env, "stranger@example.com")
  expect(refused.status).toBe(403)
  expect(refused.headers.get("cache-control")).toBe("no-store")
  expect(await refused.json()).toMatchObject({ code: "INVITE_ONLY", message: INVITE_ONLY_MESSAGE })
  // Must not leave a dead user, credential, organization, membership, or provisioning marker.
  expect(users(db)).toEqual([])
  for (const table of ["user", "account", "organization", "member", "workspace_provisioning"]) {
    expect(count(db, table)).toBe(0)
  }
  // Trailing slash and mixed-case variants are refused identically.
  const variant = await signUp(env, "stranger@example.com", { path: "/api/auth/Sign-Up/Email/" })
  expect(variant.status).toBe(403)
  expect(await variant.json()).toMatchObject({ code: "INVITE_ONLY", message: INVITE_ONLY_MESSAGE })
  expect(users(db)).toEqual([])
  // Allowlisted addresses (case-insensitive) succeed and can sign in.
  expect((await signUp(env, "owner@example.com")).status).toBe(200)
  expect((await signUp(env, "GUEST@example.com")).status).toBe(200)
  expect(users(db)).toEqual([{ email: "guest@example.com" }, { email: "owner@example.com" }])
  const signedIn = await worker.fetch(new Request(`${ORIGIN}/api/auth/sign-in/email`, {
    method: "POST",
    body: JSON.stringify({ email: "owner@example.com", password: "correct horse battery" }),
    headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": "198.51.100.251" },
  }) as Parameters<typeof worker.fetch>[0], env)
  expect(signedIn.status).toBe(200)
})

test("empty STUDIO_SIGNUP_EMAILS is invite-only and refuses every address; repeated probes still hit rate limits", async () => {
  const { db, env } = stage({ invited: "" })
  const statuses: number[] = []
  for (let attempt = 0; attempt < 7; attempt++) {
    statuses.push((await signUp(env, `probe${attempt}@example.com`, { ip: "203.0.113.42" })).status)
  }
  expect(statuses.slice(0, 5)).toEqual([403, 403, 403, 403, 403])
  expect(statuses.at(-1)).toBe(429)
  expect(users(db)).toEqual([])
})

test("without the setting sign-up stays open, as before", async () => {
  const { db, env } = stage()
  expect((await signUp(env, "anyone@example.com")).ok).toBe(true)
  expect(users(db)).toEqual([{ email: "anyone@example.com" }])
  expect(signUpEmails({})).toBeUndefined()
  expect(signUpEmails({ STUDIO_SIGNUP_EMAILS: "" })).toEqual([])
})

test("GET /api/auth/config exposes magicLink and inviteOnly without leaking allowlist or sender details", async () => {
  const readConfig = (env: Env, method = "GET") =>
    worker.fetch(new Request(`${ORIGIN}/api/auth/config`, { method }) as Parameters<typeof worker.fetch>[0], env)

  const inviteNoMail = stage({ invited: "secret-vip@example.com" }).env
  const r1 = await readConfig(inviteNoMail)
  expect(r1.status).toBe(200)
  expect(r1.headers.get("cache-control")).toBe("no-store")
  const text1 = await r1.text()
  expect(JSON.parse(text1)).toEqual({ magicLink: false, inviteOnly: true })
  expect(text1).not.toContain("secret-vip@example.com")

  const openWithMail = stage({ magicLink: true, emailFrom: "noreply@norn.cloud" }).env
  const r2 = await readConfig(openWithMail)
  expect(r2.status).toBe(200)
  const text2 = await r2.text()
  expect(JSON.parse(text2)).toEqual({ magicLink: true, inviteOnly: false })
  expect(text2).not.toContain("noreply@norn.cloud")

  // Partial or blank mail config keeps magicLink off (matches sendMagicLink availability).
  const blankFrom = stage({ magicLink: true, emailFrom: "   " }).env
  expect(await (await readConfig(blankFrom)).json()).toEqual({ magicLink: false, inviteOnly: false })
  const noBinding = stage({ emailFrom: "noreply@norn.cloud" }).env
  expect(await (await readConfig(noBinding)).json()).toEqual({ magicLink: false, inviteOnly: false })
  expect(magicLinkEnabled({ EMAIL: { send: async () => undefined }, EMAIL_FROM: "a@b.c" })).toBe(true)
  expect(magicLinkEnabled({ EMAIL_FROM: "a@b.c" })).toBe(false)

  // Non-GET is refused.
  expect((await readConfig(openWithMail, "POST")).status).toBe(405)
})
