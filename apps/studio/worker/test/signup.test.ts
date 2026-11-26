import type { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { INVITE_ONLY_MESSAGE, issueSignupInvite, SIGNUP_CLAIM_STALE_MS, SIGNUP_INVITE_HEADER, verifySignupInvite } from "../auth.ts"
import worker, { magicLinkEnabled, signUpEmails, type Env } from "../index.ts"
import { BASE_MIGRATIONS, DEFAULT_PASSWORD, DEFAULT_SECRET as SECRET, createWorkerClient, createWorkerEnv } from "./harness.ts"

const ORIGIN = "https://studio-alpha.norn.cloud"

function stage(options: { invited?: string; magicLink?: boolean; emailFrom?: string } = {}) {
  return createWorkerEnv({
    migrations: [...BASE_MIGRATIONS, "0015_signup_invite.sql"],
    origin: ORIGIN,
    assetsHtml: "studio",
    overrides: {
      ...options.invited === undefined ? {} : { STUDIO_SIGNUP_EMAILS: options.invited },
      ...options.magicLink ? { EMAIL: { send: async () => undefined }, EMAIL_FROM: options.emailFrom ?? "studio@norn.cloud" } : {},
      ...!options.magicLink && options.emailFrom !== undefined ? { EMAIL_FROM: options.emailFrom } : {},
    },
  })
}

const { post } = createWorkerClient(ORIGIN)
const invite = async (email: string, options: { now?: number; ttlMs?: number } = {}) => (await issueSignupInvite(SECRET, email, options)).token
const signUp = (env: Env, email: string, options: { path?: string; ip?: string; origin?: string; invite?: string; password?: string } = {}) =>
  post(
    env,
    options.path ?? "/api/auth/sign-up/email",
    { email, password: options.password ?? DEFAULT_PASSWORD, name: "someone" },
    {
      ...options.origin === undefined ? {} : { origin: options.origin },
      ...options.ip === undefined ? {} : { "cf-connecting-ip": options.ip },
      ...options.invite === undefined ? {} : { [SIGNUP_INVITE_HEADER]: options.invite },
    },
  )

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
  // Trailing slash and mixed-case variants cannot bypass the allowlist (403 if routed, 404 if unmatched).
  for (const path of ["/api/auth/sign-up/email/", "/api/auth/Sign-Up/Email/"]) {
    const variant = await signUp(env, "stranger@example.com", { path })
    expect([403, 404]).toContain(variant.status)
  }
  expect(users(db)).toEqual([])
  // Allowlisted addresses (case-insensitive) with their invitation succeed and can sign in.
  expect((await signUp(env, "owner@example.com", { invite: await invite("owner@example.com") })).status).toBe(200)
  expect((await signUp(env, "GUEST@example.com", { invite: await invite("Guest@Example.com") })).status).toBe(200)
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
  expect((await (await readConfig(blankFrom)).json()) as unknown).toEqual({ magicLink: false, inviteOnly: false })
  const noBinding = stage({ emailFrom: "noreply@norn.cloud" }).env
  expect((await (await readConfig(noBinding)).json()) as unknown).toEqual({ magicLink: false, inviteOnly: false })
  expect(magicLinkEnabled({ EMAIL: { send: async () => undefined }, EMAIL_FROM: "a@b.c" })).toBe(true)
  expect(magicLinkEnabled({ EMAIL_FROM: "a@b.c" })).toBe(false)

  // Non-GET is refused.
  expect((await readConfig(openWithMail, "POST")).status).toBe(405)
})

test("an accepted-duplicate sign-up on an invite-only stage keeps the existing account unchanged and leaks nothing", async () => {
  const { db, env } = stage({ invited: "owner@example.com" })
  const token = await invite("owner@example.com")
  const first = await signUp(env, "owner@example.com", { invite: token })
  expect(first.status).toBe(200)
  expect(first.headers.getSetCookie()).toEqual([])
  const firstBody = await first.json() as { token: unknown; user?: { email?: string } }
  expect(firstBody.token).toBeNull()

  // Re-submitting the same allowlisted address with a different password returns 200 with no session
  // and must not create a second user or overwrite the existing password.
  const duplicate = await worker.fetch(new Request(`${ORIGIN}/api/auth/sign-up/email`, {
    method: "POST",
    body: JSON.stringify({ email: "OWNER@example.com", password: "a-different-password-9999", name: "Someone Else" }),
    headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": "198.51.100.240", [SIGNUP_INVITE_HEADER]: token },
  }) as Parameters<typeof worker.fetch>[0], env)
  expect(duplicate.status).toBe(200)
  expect(duplicate.headers.getSetCookie()).toEqual([])
  expect(users(db)).toEqual([{ email: "owner@example.com" }])

  // Original password still signs in; the duplicate attempt's password does not.
  const wrongPw = await worker.fetch(new Request(`${ORIGIN}/api/auth/sign-in/email`, {
    method: "POST",
    body: JSON.stringify({ email: "owner@example.com", password: "a-different-password-9999" }),
    headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": "198.51.100.241" },
  }) as Parameters<typeof worker.fetch>[0], env)
  expect(wrongPw.status).toBe(401)
  const rightPw = await worker.fetch(new Request(`${ORIGIN}/api/auth/sign-in/email`, {
    method: "POST",
    body: JSON.stringify({ email: "owner@example.com", password: "correct horse battery" }),
    headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": "198.51.100.242" },
  }) as Parameters<typeof worker.fetch>[0], env)
  expect(rightPw.status).toBe(200)
})

test("knowing an invited address is not enough: sign-up needs that address's own unspent, unexpired invitation", async () => {
  const { db, env } = stage({ invited: "owner@example.com,guest@example.com" })
  const refusedWith = async (response: Response) => {
    expect(response.status).toBe(403)
    expect(await response.json()).toMatchObject({ code: "INVITE_ONLY", message: INVITE_ONLY_MESSAGE })
  }
  // No invitation, a malformed one, another address's, an expired one, a forged one: one refusal, nothing written.
  await refusedWith(await signUp(env, "owner@example.com"))
  await refusedWith(await signUp(env, "owner@example.com", { invite: "v1.not-a-token" }))
  await refusedWith(await signUp(env, "owner@example.com", { invite: await invite("guest@example.com") }))
  await refusedWith(await signUp(env, "owner@example.com", { invite: await invite("owner@example.com", { now: Date.now() - 2 * 60_000, ttlMs: 60_000 }) }))
  const forged = (await issueSignupInvite("f".repeat(40), "owner@example.com")).token
  await refusedWith(await signUp(env, "owner@example.com", { invite: forged }))
  // An invitation for an address that is no longer invited is refused too.
  await refusedWith(await signUp(env, "former@example.com", { invite: await invite("former@example.com") }))
  for (const table of ["user", "account", "organization", "member", "signup_invite_use"]) expect(count(db, table)).toBe(0)

  // A refused sign-up (password too short) does not spend the invitation.
  const token = await invite("owner@example.com")
  expect((await signUp(env, "owner@example.com", { invite: token, password: "short" })).status).toBe(400)
  expect(count(db, "signup_invite_use")).toBe(0)
  // The invitation creates exactly one account; it cannot create another, even for an address whose account was removed.
  expect((await signUp(env, "owner@example.com", { invite: token })).status).toBe(200)
  expect(users(db)).toEqual([{ email: "owner@example.com" }])
  expect(db.query(`select "email", "consumed_at" is not null as consumed from "signup_invite_use"`).all()).toEqual([{ email: "owner@example.com", consumed: 1 }])
  db.exec(`delete from "user"`)
  // A spent invitation is refused when the account would be written; Better Auth answers that like an
  // existing address (200, no session), so the answer reveals nothing, and no account is created.
  const spent = await signUp(env, "owner@example.com", { invite: token })
  expect(spent.headers.getSetCookie()).toEqual([])
  expect(users(db)).toEqual([])
})

test("two sign-ups racing with one invitation create one account", async () => {
  const { db, env } = stage({ invited: "owner@example.com" })
  const token = await invite("owner@example.com")
  const results = await Promise.all([signUp(env, "owner@example.com", { invite: token }), signUp(env, "owner@example.com", { invite: token }), signUp(env, "owner@example.com", { invite: token })])
  expect(results.map((r) => r.status).filter((status) => status === 200).length).toBeGreaterThanOrEqual(1)
  expect(users(db)).toEqual([{ email: "owner@example.com" }])
  expect(count(db, "signup_invite_use")).toBe(1)
})

test("an invitation is bound to its address and expiry by the deployment's secret", async () => {
  const now = Date.UTC(2026, 10, 25)
  const { token, expiresAt } = await issueSignupInvite(SECRET, " Owner@Example.com ", { now, ttlMs: 60_000 })
  expect(expiresAt).toBe(now + 60_000)
  expect(await verifySignupInvite(SECRET, token, "owner@example.com", now)).toMatchObject({ expiresAt })
  expect(await verifySignupInvite(SECRET, token, "owner@example.com", now + 60_000)).toBeNull()
  expect(await verifySignupInvite(SECRET, token, "other@example.com", now)).toBeNull()
  expect(await verifySignupInvite("x".repeat(40), token, "owner@example.com", now)).toBeNull()
  const [version, expiry, nonce, signature] = token.split(".")
  expect(await verifySignupInvite(SECRET, [version, String(Number(expiry) + 1), nonce, signature].join("."), "owner@example.com", now)).toBeNull()
  // A 30-day invitation from an issuer whose clock runs a few minutes ahead is still accepted.
  const ahead = await issueSignupInvite(SECRET, "owner@example.com", { now: now + 3 * 60_000, ttlMs: 30 * 24 * 60 * 60_000 })
  expect(await verifySignupInvite(SECRET, ahead.token, "owner@example.com", now)).not.toBeNull()
  const tooFar = await issueSignupInvite(SECRET, "owner@example.com", { now: now + 6 * 60_000, ttlMs: 30 * 24 * 60 * 60_000 })
  expect(await verifySignupInvite(SECRET, tooFar.token, "owner@example.com", now)).toBeNull()
  // An expiry further out than the 30-day maximum is refused even when correctly signed.
  const far = await issueSignupInvite(SECRET, "owner@example.com", { now: now + 10 * 24 * 60 * 60_000, ttlMs: 30 * 24 * 60 * 60_000 })
  expect(await verifySignupInvite(SECRET, far.token, "owner@example.com", now)).toBeNull()
  // The signature is made with a key derived for invitations, not the raw auth secret.
  const [, , rawNonce] = token.split(".")
  const rawKey = await crypto.subtle.importKey("raw", new TextEncoder().encode(SECRET), { name: "HMAC", hash: "SHA-256" }, false, ["sign"])
  const rawSignature = Buffer.from(await crypto.subtle.sign("HMAC", rawKey, new TextEncoder().encode(`norn.signup-invite.v1|owner@example.com|${expiresAt}|${rawNonce}`))).toString("base64url")
  expect(token.endsWith(rawSignature)).toBe(false)
  expect(() => issueSignupInvite(SECRET, "not-an-address")).toThrow()
  await expect(issueSignupInvite(SECRET, "a@b.co", { ttlMs: 31 * 24 * 60 * 60_000 })).rejects.toThrow("30 days")
})

test("the operator's invite command prints a single-use link the deployment accepts", async () => {
  const { inviteLink, parseArgs } = await import("../../scripts/signup-invite.ts")
  const { db, env } = stage({ invited: "ada@example.com" })
  const printed = await inviteLink(SECRET, ["--url", ORIGIN, "--days", "2", "Ada@Example.com"])
  const link = new URL(printed.split("\n")[0] ?? "")
  expect(link.origin).toBe(ORIGIN)
  expect(printed).toContain("for ada@example.com, single use")
  // The invitation rides in the fragment, which browsers never send to a server.
  expect(link.search).toBe("")
  expect((await signUp(env, "ada@example.com", { invite: new URLSearchParams(link.hash.slice(1)).get("invite") ?? "" })).status).toBe(200)
  expect(users(db)).toEqual([{ email: "ada@example.com" }])
  await expect(inviteLink(undefined, ["--url", ORIGIN, "a@b.co"])).rejects.toThrow("BETTER_AUTH_SECRET")
  expect(() => parseArgs(["a@b.co"])).toThrow("usage")
  expect(() => parseArgs(["--url", ORIGIN, "--days", "31", "a@b.co"])).toThrow("--days")
})

test("a claim left by a request that died is taken over after five minutes, and the dead request's user can never be written", async () => {
  const { db, env } = stage({ invited: "owner@example.com" })
  const token = await invite("owner@example.com")
  const nonce = token.split(".")[2] ?? ""
  const nonceHash = Buffer.from(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(nonce))).toString("hex")
  // A fresh claim (another request is writing the user right now) is held: this sign-up writes nothing.
  db.run(`insert into "signup_invite_use" ("nonce_hash", "email", "user_id", "claimed_at") values (?, 'owner@example.com', 'u_dead', ?)`, [nonceHash, Date.now()])
  await signUp(env, "owner@example.com", { invite: token })
  expect(users(db)).toEqual([])
  // Stale: taken over, and the account is created under the new claim.
  db.run(`update "signup_invite_use" set "claimed_at" = ? where "nonce_hash" = ?`, [Date.now() - SIGNUP_CLAIM_STALE_MS - 1000, nonceHash])
  expect((await signUp(env, "owner@example.com", { invite: token })).status).toBe(200)
  expect(users(db)).toEqual([{ email: "owner@example.com" }])
  // The superseded request, waking up, cannot insert its user; the consumed claim never moves again.
  expect(() => db.run(`insert into "user" ("id", "name", "email", "emailVerified", "createdAt", "updatedAt") values ('u_dead', 'x', 'other@example.com', 0, '2026-01-01', '2026-01-01')`)).toThrow("signup.invite_superseded")
  expect(() => db.run(`update "signup_invite_use" set "user_id" = 'u_late', "claimed_at" = 0 where "nonce_hash" = ?`, [nonceHash])).toThrow("signup.invite_consumed")
})
