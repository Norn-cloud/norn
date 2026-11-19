import { Database } from "bun:sqlite"
import { afterEach, beforeEach, expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { getMigrations } from "better-auth/db/migration"
import worker, { type Env } from "../index.ts"
import { authOptions, createAuth } from "../auth.ts"
import { forgetPolarCustomer } from "../polar.ts"
import {
  applySubscription, billingConfig, BILLING_ENV, entitlementOf, limitRaised, ownedWorkspaces, parsePlans, PLACEHOLDER_PLANS, statusOf, syncPlans, UNLIMITED, workspaceMembers,
  type BillingConfig, type BillingDb, type PlanTable, type PolarSubscription,
} from "../billing.ts"
import { celldConfig, celldStudio } from "../../celld-config.ts"
import { README, withBillingSection } from "../../scripts/billing-readme.ts"

// Polar is mocked throughout: no request leaves the test. The Worker runs against bun:sqlite with
// Studio's committed migrations, so the entitlement record is the one D1 holds.

const ORIGIN = "https://studio-preview.norn.cloud"
const MIGRATIONS = join(import.meta.dir, "../migrations")
const PLANS: PlanTable = {
  free: { cells: 1, members: 2 },
  plans: [
    { id: "pro", name: "Pro", productId: "prod_pro", limits: { cells: 3, members: 10 } },
    { id: "team", name: "Team", productId: "prod_team", limits: { cells: null, members: null } },
  ],
}
// A Standard Webhooks secret as Polar issues it: `whsec_` + base64 key bytes.
const KEY = new TextEncoder().encode("polar-webhook-test-key-0123456789")
const WEBHOOK_SECRET = `whsec_${btoa(String.fromCharCode(...KEY))}`
const POLAR_ENV = { POLAR_ACCESS_TOKEN: "polar_oat_test", POLAR_WEBHOOK_SECRET: WEBHOOK_SECRET, POLAR_SERVER: "sandbox", POLAR_PLANS: JSON.stringify(PLANS) }
const CONFIG = billingConfig(POLAR_ENV) as BillingConfig

function database(): Database {
  const db = new Database(":memory:")
  for (const file of readdirSync(MIGRATIONS).filter((name) => name.endsWith(".sql")).sort()) db.exec(readFileSync(join(MIGRATIONS, file), "utf8"))
  return db
}
function stage(billing: boolean, plans: PlanTable = PLANS): Env {
  const assets = { fetch: async () => new Response("<!doctype html>studio") } as unknown as Fetcher
  return { ASSETS: assets, AUTH_DB: database() as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip", ...billing ? { ...POLAR_ENV, POLAR_PLANS: JSON.stringify(plans) } : {} }
}
const sql = (env: Env) => env.AUTH_DB as unknown as Database

// --- the Polar API mock --------------------------------------------------------------------------
interface PolarCall { readonly method: string; readonly path: string; readonly search: string; readonly body: unknown }
let polarCalls: PolarCall[] = []
const realFetch = globalThis.fetch
beforeEach(() => {
  polarCalls = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init)
    const url = new URL(request.url)
    if (url.origin !== "https://sandbox-api.polar.sh") throw new Error(`unexpected fetch ${url.origin}`)
    const text = await request.text()
    const body = text === "" ? null : JSON.parse(text)
    polarCalls.push({ method: request.method, path: url.pathname, search: url.search, body })
    if (request.method === "GET" && url.pathname === "/v1/customers/") return Response.json({ items: [], pagination: { total_count: 0, max_page: 1 } })
    if (request.method === "POST" && url.pathname === "/v1/customers/") return Response.json({ id: "cus_1", type: "individual", external_id: body.external_id }, { status: 201 })
    if (request.method === "PATCH" && url.pathname.startsWith("/v1/customers/external/")) return Response.json({ id: "cus_1" })
    if (request.method === "POST" && url.pathname === "/v1/checkouts/") return Response.json({ id: "chk_1", url: "https://sandbox.polar.sh/checkout/chk_1", client_secret: "cs_1" }, { status: 201 })
    if (request.method === "POST" && url.pathname === "/v1/customer-sessions/") return Response.json({ id: "cs_1", customer_portal_url: "https://sandbox.polar.sh/portal/cs_1" }, { status: 201 })
    return Response.json({ detail: "not mocked" }, { status: 404 })
  }) as typeof fetch
})
afterEach(() => { globalThis.fetch = realFetch })

// --- Worker calls ------------------------------------------------------------------------------
let address = 0
const call = (env: Env, path: string, init: RequestInit = {}) => {
  const headers = new Headers(init.headers)
  if (!headers.has("cf-connecting-ip")) headers.set("cf-connecting-ip", `198.51.100.${++address % 250}`)
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }) as Parameters<typeof worker.fetch>[0], env)
}
const post = (env: Env, path: string, body: unknown, headers: HeadersInit = {}) =>
  call(env, path, { method: "POST", body: JSON.stringify(body), headers: { "content-type": "application/json", origin: ORIGIN, ...headers } })
const cookieOf = (response: Response) => response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
async function enroll(env: Env, email: string): Promise<{ cookie: string; userId: string }> {
  expect((await post(env, "/api/auth/sign-up/email", { email, password: "correct horse battery", name: email.split("@")[0] })).status).toBe(200)
  const signIn = await post(env, "/api/auth/sign-in/email", { email, password: "correct horse battery" })
  expect(signIn.status).toBe(200)
  const { id } = sql(env).query("select id from user where email = ?").get(email) as { id: string }
  return { cookie: cookieOf(signIn), userId: id }
}

// --- signed webhooks ---------------------------------------------------------------------------
async function sign(id: string, timestamp: number, body: string, key: Uint8Array = KEY): Promise<string> {
  const hmac = await crypto.subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"])
  const signature = new Uint8Array(await crypto.subtle.sign("HMAC", hmac, new TextEncoder().encode(`${id}.${timestamp}.${body}`)))
  return `v1,${btoa(String.fromCharCode(...signature))}`
}
function subscription(overrides: Partial<{ id: string; status: string; product_id: string; modified_at: string; external_id: string | null }>): Record<string, unknown> {
  const at = overrides.modified_at ?? "2026-11-19T10:00:00Z"
  return {
    id: overrides.id ?? "sub_1", status: overrides.status ?? "active", product_id: overrides.product_id ?? "prod_pro",
    created_at: "2026-11-19T09:00:00Z", modified_at: at, amount: 2000, currency: "usd", recurring_interval: "month", recurring_interval_count: 1,
    current_period_start: at, current_period_end: "2026-12-19T09:00:00Z", cancel_at_period_end: false, customer_id: "cus_1",
    customer: { id: "cus_1", external_id: overrides.external_id === undefined ? "user_1" : overrides.external_id, type: "individual" },
    metadata: {},
  }
}
async function webhook(env: Env, type: string, data: Record<string, unknown>, options: { id?: string; at?: number; key?: Uint8Array } = {}): Promise<Response> {
  const body = JSON.stringify({ type, timestamp: new Date().toISOString(), data })
  const id = options.id ?? `msg_${crypto.randomUUID()}`
  const at = options.at ?? Math.floor(Date.now() / 1000)
  return call(env, "/api/auth/polar/webhooks", { method: "POST", body, headers: { "content-type": "application/json", "webhook-id": id, "webhook-timestamp": String(at), "webhook-signature": await sign(id, at, body, options.key) } })
}
const row = (env: Env, id = "sub_1") => sql(env).query(`select subject_id, product_id, status, polar_status, version, updated_at from billing_subscription where id = ?`).get(id) as { subject_id: string; product_id: string; status: string; polar_status: string; version: number; updated_at: number } | null

// ===============================================================================================
// Configuration

test("billing is off without Polar settings, and half a configuration refuses", () => {
  expect(billingConfig({})).toBeUndefined()
  expect(billingConfig({ POLAR_ACCESS_TOKEN: undefined, STUDIO_MCP: "on" })).toBeUndefined()
  expect(() => billingConfig({ POLAR_ACCESS_TOKEN: "t" })).toThrow("go together")
  expect(() => billingConfig({ ...POLAR_ENV, POLAR_WEBHOOK_SECRET: "" })).toThrow("POLAR_WEBHOOK_SECRET")
  expect(() => billingConfig({ ...POLAR_ENV, POLAR_SERVER: "live" })).toThrow("POLAR_SERVER")
  expect(CONFIG).toMatchObject({ server: "sandbox", table: PLANS })
})

test("a declared but empty billing setting refuses instead of turning billing off", () => {
  // Declared at all means billing was meant to be on; empty is not a value.
  expect(() => billingConfig({ POLAR_ACCESS_TOKEN: "" })).toThrow("POLAR_ACCESS_TOKEN")
  expect(() => billingConfig({ POLAR_ACCESS_TOKEN: "", POLAR_WEBHOOK_SECRET: "", POLAR_SERVER: "", POLAR_PLANS: "" })).toThrow("go together")
  expect(() => billingConfig({ ...POLAR_ENV, POLAR_ACCESS_TOKEN: "  " })).toThrow("POLAR_ACCESS_TOKEN")
})

test("the plan table refuses anything it would have to guess", () => {
  expect(parsePlans(JSON.stringify(PLACEHOLDER_PLANS))).toEqual(PLACEHOLDER_PLANS)
  expect(() => parsePlans("{")).toThrow("JSON")
  expect(() => parsePlans(JSON.stringify({ free: { cells: 1, members: 1 } }))).toThrow("plans")
  expect(() => parsePlans(JSON.stringify({ free: { cells: -1, members: 1 }, plans: [] }))).toThrow("free.cells")
  expect(() => parsePlans(JSON.stringify({ free: { cells: 1 }, plans: [] }))).toThrow("free.members")
  expect(() => parsePlans(JSON.stringify({ ...PLANS, plans: [{ ...PLANS.plans[0], id: "Pro!" }] }))).toThrow("slug")
  expect(() => parsePlans(JSON.stringify({ ...PLANS, plans: [{ ...PLANS.plans[0], id: "free" }] }))).toThrow("reserved")
  expect(() => parsePlans(JSON.stringify({ ...PLANS, plans: [{ ...PLANS.plans[0], productId: "" }] }))).toThrow("productId")
  expect(() => parsePlans(JSON.stringify({ ...PLANS, plans: [PLANS.plans[0], { ...PLANS.plans[1], productId: "prod_pro" }] }))).toThrow("product ids")
  expect(() => parsePlans(JSON.stringify({ ...PLANS, plans: [PLANS.plans[0], { ...PLANS.plans[1], id: "pro" }] }))).toThrow("plan ids")
})

test("the README's billing section is generated from the code's settings and placeholder plans", () => {
  const readme = readFileSync(README, "utf8")
  expect(readme).toBe(withBillingSection(readme))
  for (const name of BILLING_ENV) expect(readme).toContain(`\`${name}\``)
  expect(readme).toContain(JSON.stringify(PLACEHOLDER_PLANS, null, 2))
})

test("celld carries the Polar settings together, or refuses", () => {
  const BASE = { STUDIO_STAGE: "demo", STUDIO_BUILD: "a".repeat(40), BETTER_AUTH_URL: "https://studio-demo.norn.cloud", BETTER_AUTH_SECRET: "s".repeat(40), STUDIO_CLIENT_IP_HEADER: "x-real-ip" }
  expect((celldConfig(celldStudio(BASE)).vars as Record<string, string>).POLAR_ACCESS_TOKEN).toBeUndefined()
  const vars = celldConfig(celldStudio({ ...BASE, ...POLAR_ENV })).vars as Record<string, string>
  expect(vars).toMatchObject({ POLAR_ACCESS_TOKEN: "polar_oat_test", POLAR_WEBHOOK_SECRET: WEBHOOK_SECRET, POLAR_SERVER: "sandbox" })
  expect(JSON.parse(vars.POLAR_PLANS as string)).toEqual(PLANS)
  expect(() => celldStudio({ ...BASE, POLAR_ACCESS_TOKEN: "t" })).toThrow("go together")
})

test("Polar's plugin adds no Better Auth tables: 0001 and 0009 are the whole schema", async () => {
  const db = new Database(":memory:")
  db.exec(readFileSync(join(MIGRATIONS, "0001_better_auth.sql"), "utf8"))
  const options = authOptions({ clientIpHeader: "cf-connecting-ip", database: db, secret: "schema-generation-only-not-a-secret", baseURL: "https://studio.invalid", billing: { config: CONFIG, db: db as unknown as BillingDb } })
  const { toBeAdded, toBeCreated } = await getMigrations(options)
  expect([...toBeAdded, ...toBeCreated]).toEqual([])
})

// ===============================================================================================
// Entitlement transitions (the record the webhooks write)

test("Polar statuses map to active, past due or canceled; nothing unknown is granted", () => {
  expect(["active", "trialing"].map(statusOf)).toEqual(["active", "active"])
  // Only past due is grace; unpaid means Polar's retries are exhausted.
  expect(statusOf("past_due")).toBe("past_due")
  expect(["unpaid", "canceled", "incomplete", "incomplete_expired", "paused", "something_new"].map(statusOf)).toEqual(Array(6).fill("canceled"))
})

test("a subscription moves free → active → past due → canceled, and an older event never rolls it back", async () => {
  const db = database() as unknown as BillingDb
  const sub = (status: string, modified_at: string, product_id = "prod_pro") => subscription({ status, modified_at, product_id }) as unknown as PolarSubscription
  expect(await entitlementOf(db, CONFIG, "user_1")).toEqual({ billing: "enabled", plan: "free", status: "none", limits: PLANS.free })
  expect(await applySubscription(db, sub("active", "2026-11-19T10:00:00Z"), 1)).toBe("applied")
  expect(await entitlementOf(db, CONFIG, "user_1")).toEqual({ billing: "enabled", plan: "pro", status: "active", limits: { cells: 3, members: 10 } })
  // Past due keeps the plan while Polar retries the payment.
  expect(await applySubscription(db, sub("past_due", "2026-11-19T11:00:00Z"), 2)).toBe("applied")
  expect(await entitlementOf(db, CONFIG, "user_1")).toMatchObject({ plan: "pro", status: "past_due" })
  expect(await applySubscription(db, sub("canceled", "2026-11-19T12:00:00Z"), 3)).toBe("applied")
  expect(await entitlementOf(db, CONFIG, "user_1")).toEqual({ billing: "enabled", plan: "free", status: "canceled", limits: PLANS.free })
  // The earlier "active" event delivered again (or late) is stale: the plan stays canceled.
  expect(await applySubscription(db, sub("active", "2026-11-19T10:00:00Z"), 4)).toBe("stale")
  expect(await entitlementOf(db, CONFIG, "user_1")).toMatchObject({ plan: "free", status: "canceled" })
  // A newer renewal reinstates it.
  expect(await applySubscription(db, sub("active", "2026-11-20T10:00:00Z"), 5)).toBe("applied")
  expect(await entitlementOf(db, CONFIG, "user_1")).toMatchObject({ plan: "pro", status: "active" })
})

test("the richest live plan decides; unknown products and customers without our id grant nothing", async () => {
  const db = database() as unknown as BillingDb
  await applySubscription(db, subscription({ id: "sub_pro" }) as unknown as PolarSubscription, 1)
  await applySubscription(db, subscription({ id: "sub_team", product_id: "prod_team" }) as unknown as PolarSubscription, 1)
  expect(await entitlementOf(db, CONFIG, "user_1")).toMatchObject({ plan: "team", limits: { cells: null, members: null } })
  expect(await applySubscription(db, subscription({ id: "sub_x", product_id: "prod_unknown", external_id: "user_2" }) as unknown as PolarSubscription, 1)).toBe("applied")
  expect(await entitlementOf(db, CONFIG, "user_2")).toMatchObject({ plan: "free", limits: PLANS.free })
  expect(await applySubscription(db, subscription({ id: "sub_y", external_id: null }) as unknown as PolarSubscription, 1)).toBe("ignored")
  expect(await entitlementOf(db, undefined, "user_1")).toEqual(UNLIMITED)
})

// ===============================================================================================
// Webhooks through the Worker: signature, window, replay

test("a correctly signed subscription webhook updates the entitlement record", async () => {
  const env = stage(true)
  const response = await webhook(env, "subscription.active", subscription({}))
  expect(response.status).toBe(200)
  expect(row(env)).toMatchObject({ subject_id: "user_1", product_id: "prod_pro", status: "active", polar_status: "active" })
  // Other event types are acknowledged and change nothing.
  expect((await webhook(env, "order.paid", { id: "ord_1" })).status).toBeLessThan(500)
})

test("a webhook with a bad signature, another key, a missing header or an old timestamp is refused and applies nothing", async () => {
  const env = stage(true)
  const body = JSON.stringify({ type: "subscription.active", timestamp: new Date().toISOString(), data: subscription({}) })
  const at = Math.floor(Date.now() / 1000)
  const forged = await call(env, "/api/auth/polar/webhooks", { method: "POST", body, headers: { "content-type": "application/json", "webhook-id": "msg_forged", "webhook-timestamp": String(at), "webhook-signature": `v1,${btoa("not the signature")}` } })
  expect(forged.status).toBe(403)
  expect((await webhook(env, "subscription.active", subscription({}), { key: new TextEncoder().encode("another-key-entirely-0123456789ab") })).status).toBe(403)
  const unsigned = await call(env, "/api/auth/polar/webhooks", { method: "POST", body, headers: { "content-type": "application/json", "webhook-id": "msg_unsigned", "webhook-timestamp": String(at) } })
  expect(unsigned.status).toBe(403)
  // A captured delivery replayed after the 5-minute window fails verification.
  expect((await webhook(env, "subscription.active", subscription({}), { at: at - 10 * 60 })).status).toBe(403)
  // A body changed after signing fails too.
  const id = "msg_tampered"
  const tampered = await call(env, "/api/auth/polar/webhooks", { method: "POST", body: body.replace("prod_pro", "prod_team"), headers: { "content-type": "application/json", "webhook-id": id, "webhook-timestamp": String(at), "webhook-signature": await sign(id, at, body) } })
  expect(tampered.status).toBe(403)
  expect(row(env)).toBeNull()
  expect(sql(env).query("select count(*) as n from billing_webhook").get()).toEqual({ n: 0 })
})

test("a replayed delivery inside the window is acknowledged without being applied again", async () => {
  const env = stage(true)
  const at = Math.floor(Date.now() / 1000)
  expect((await webhook(env, "subscription.active", subscription({}), { id: "msg_once", at })).status).toBe(200)
  sql(env).run("update billing_subscription set updated_at = 0")
  const replay = await webhook(env, "subscription.active", subscription({}), { id: "msg_once", at })
  expect(replay.status).toBe(200)
  expect(row(env)?.updated_at).toBe(0)
  // Reordered deliveries: the newer cancel arrives first; the older activation that follows is stale.
  await webhook(env, "subscription.canceled", subscription({ id: "sub_2", status: "canceled", modified_at: "2026-11-19T12:00:00Z" }))
  await webhook(env, "subscription.active", subscription({ id: "sub_2", status: "active", modified_at: "2026-11-19T11:00:00Z" }))
  expect(row(env, "sub_2")?.status).toBe("canceled")
})

// ===============================================================================================
// Enforcement where workspaces (hosted cells) and members are created

test("an unpaid subscription drops to the free plan", async () => {
  const env = stage(true)
  await webhook(env, "subscription.active", subscription({ modified_at: "2026-11-19T10:00:00Z" }))
  await webhook(env, "subscription.updated", subscription({ status: "unpaid", modified_at: "2026-11-19T11:00:00Z" }))
  expect(row(env)).toMatchObject({ status: "canceled", polar_status: "unpaid" })
  expect(await entitlementOf(env.AUTH_DB as unknown as BillingDb, CONFIG, "user_1")).toMatchObject({ plan: "free", status: "canceled" })
})

test("sign-up makes no Polar call: an existing Polar customer with the same email is never linked to a new account", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "cora@example.com")
  expect(polarCalls).toEqual([])
  // The customer is made at checkout, under the account's own id, not looked up by email.
  expect((await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie } })).status).toBe(303)
  expect(polarCalls.map((entry) => `${entry.method} ${entry.path}`)).toEqual(["POST /v1/checkouts/"])
  expect(polarCalls[0]?.body).toMatchObject({ external_customer_id: userId })
})

test("deleting an account deletes its Polar customer, best effort", async () => {
  const db = database()
  const billing = { config: CONFIG, db: db as unknown as BillingDb }
  const auth = createAuth({ clientIpHeader: "cf-connecting-ip", database: db, secret: "t".repeat(40), baseURL: ORIGIN, billing })
  const context = await auth.$context
  const user = { id: "user_del" }
  db.run(`insert into user (id, name, email, emailVerified, createdAt, updatedAt) values ('user_del', 'del', 'del@example.com', 0, ?, ?)`, [new Date().toISOString(), new Date().toISOString()])
  await context.internalAdapter.deleteUser(user.id)
  expect(polarCalls.map((entry) => `${entry.method} ${entry.path}${entry.search}`)).toEqual([`DELETE /v1/customers/external/${user.id}?anonymize=true`])
  // Polar answering 404 (never checked out) or failing never blocks the deletion.
  await forgetPolarCustomer(billing)({ id: "user_without_customer" })
})

test("a workspace over the plan's cell limit is refused with a typed refusal and an upgrade link, and admitted after upgrading", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "ada@example.com")
  // Sign-up provisioned one owned workspace: the free plan's one hosted cell.
  const refused = await post(env, "/api/auth/organization/create", { name: "Second", slug: "second" }, { cookie })
  expect(refused.status).toBe(403)
  const body = await refused.json() as { code: string; message: string; refusal: Record<string, unknown> }
  expect(body.code).toBe("PLAN_LIMIT")
  expect(body.refusal).toEqual({ kind: "plan-limit", limit: "cells", plan: "free", allowed: 1, used: 1, upgradeUrl: `${ORIGIN}/api/billing/checkout?plan=pro`, upgradeBy: "you" })
  expect(sql(env).query("select count(*) as n from organization").get()).toEqual({ n: 1 })
  // Polar reports the subscription; the same request is now admitted.
  expect((await webhook(env, "subscription.active", subscription({ external_id: userId }))).status).toBe(200)
  expect((await post(env, "/api/auth/organization/create", { name: "Second", slug: "second" }, { cookie })).status).toBe(200)
  // And canceled again, the third is refused; a paying person's upgrade link was the portal, a free one's a checkout.
  expect((await post(env, "/api/auth/organization/create", { name: "Third", slug: "third" }, { cookie })).status).toBe(200)
  const atLimit = await (await post(env, "/api/auth/organization/create", { name: "Fourth", slug: "fourth" }, { cookie })).json() as { refusal: { upgradeUrl: string; plan: string } }
  expect(atLimit.refusal).toMatchObject({ plan: "pro", upgradeUrl: `${ORIGIN}/api/billing/portal` })
})

test("invitations past the workspace owner's member limit are refused", async () => {
  const env = stage(true)
  const { cookie } = await enroll(env, "lea@example.com")
  const { id: organizationId } = sql(env).query("select organizationId as id from member").get() as { id: string }
  // Free: two seats, the owner holds one.
  expect((await post(env, "/api/auth/organization/invite-member", { email: "one@example.com", role: "member", organizationId }, { cookie })).status).toBe(200)
  const refused = await post(env, "/api/auth/organization/invite-member", { email: "two@example.com", role: "member", organizationId }, { cookie })
  expect(refused.status).toBe(403)
  expect(((await refused.json()) as { refusal: { limit: string } }).refusal).toMatchObject({ kind: "plan-limit", limit: "members", allowed: 2, used: 2, upgradeBy: "you", upgradeUrl: `${ORIGIN}/api/billing/checkout?plan=pro` })
})

/** Put a second person into a workspace directly (accepting an invitation needs a verified address). */
function join_(env: Env, organizationId: string, userId: string, role: string): string {
  const id = `mem_${crypto.randomUUID()}`
  sql(env).run(`insert into member (id, organizationId, userId, role, createdAt) values (?, ?, ?, ?, ?)`, [id, organizationId, userId, role, new Date().toISOString()])
  return id
}
const firstWorkspace = (env: Env, userId: string) => (sql(env).query("select organizationId as id from member where userId = ? order by createdAt limit 1").get(userId) as { id: string }).id

test("handing the owner role to someone else and stepping down does not free a workspace: the payer is fixed at creation", async () => {
  const env = stage(true)
  const ada = await enroll(env, "ada2@example.com")
  const bo = await enroll(env, "bo@example.com")
  const workspace = firstWorkspace(env, ada.userId)
  const boMember = join_(env, workspace, bo.userId, "member")
  const adaMember = (sql(env).query("select id from member where organizationId = ? and userId = ?").get(workspace, ada.userId) as { id: string }).id
  // Ada promotes Bo to owner; Bo demotes Ada. Ada no longer owns anything by role.
  expect((await post(env, "/api/auth/organization/update-member-role", { memberId: boMember, role: "owner", organizationId: workspace }, { cookie: ada.cookie })).status).toBe(200)
  expect((await post(env, "/api/auth/organization/update-member-role", { memberId: adaMember, role: "member", organizationId: workspace }, { cookie: bo.cookie })).status).toBe(200)
  expect(sql(env).query(`select count(*) as n from member where userId = ? and role like '%owner%'`).get(ada.userId)).toEqual({ n: 0 })
  // She still pays for it: a second workspace is refused, and her billing document still counts it.
  const refused = await post(env, "/api/auth/organization/create", { name: "Again", slug: "again" }, { cookie: ada.cookie })
  expect(refused.status).toBe(403)
  expect(((await refused.json()) as { refusal: unknown }).refusal).toMatchObject({ limit: "cells", allowed: 1, used: 1 })
  expect(((await (await call(env, "/api/billing", { headers: { cookie: ada.cookie } })).json()) as { usage: unknown }).usage).toEqual({ cells: 1 })
  // Bo's new owner role adds nothing to what he pays for: still his own workspace only.
  expect(((await (await call(env, "/api/billing", { headers: { cookie: bo.cookie } })).json()) as { usage: unknown }).usage).toEqual({ cells: 1 })
})

test("concurrent workspace creations cannot pass the cell limit together: the ones past it are refused", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "rush@example.com")
  await webhook(env, "subscription.active", subscription({ external_id: userId }))
  // Pro allows 3; one exists. Four at once: exactly two fit.
  const responses = await Promise.all(["a", "b", "c", "d"].map((name) => post(env, "/api/auth/organization/create", { name, slug: `rush-${name}` }, { cookie })))
  expect(responses.map((response) => response.status).sort()).toEqual([200, 200, 403, 403])
  for (const response of responses.filter((one) => one.status === 403)) {
    expect(((await response.json()) as { code: string; refusal: unknown })).toMatchObject({ code: "PLAN_LIMIT", refusal: { limit: "cells", plan: "pro", allowed: 3, used: 3, upgradeBy: "you" } })
  }
  expect(sql(env).query("select count(*) as n from billing_workspace where payer_id = ?").get(userId)).toEqual({ n: 3 })
  expect(sql(env).query("select count(*) as n from organization").get()).toEqual({ n: 3 })
  expect(sql(env).query("select count(*) as n from member where userId = ?").get(userId)).toEqual({ n: 3 })
})

test("a workspace over the cell limit is never written, so nothing has to be undone", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "nounder@example.com")
  // The free cell is open (the provisioned workspace is gone), so the whole burst races for it.
  const provisioned = firstWorkspace(env, userId)
  sql(env).run(`delete from member where organizationId = ?`, [provisioned])
  sql(env).run(`delete from organization where id = ?`, [provisioned])
  // Nothing can be deleted: a creation that wrote and then undid a workspace would leave it behind.
  for (const table of ["organization", "member", "billing_workspace", "invitation"]) sql(env).run(`create trigger no_delete_${table} before delete on "${table}" begin select raise(abort, 'no deletes'); end`)
  const responses = await Promise.all(["a", "b", "c"].map((name) => post(env, "/api/auth/organization/create", { name, slug: `nounder-${name}` }, { cookie })))
  expect(responses.map((response) => response.status).sort()).toEqual([200, 403, 403])
  expect(sql(env).query("select count(*) as n from organization").get()).toEqual({ n: 1 })
  expect(sql(env).query("select count(*) as n from member where userId = ?").get(userId)).toEqual({ n: 1 })
  expect(sql(env).query("select count(*) as n from billing_workspace").get()).toEqual({ n: 1 })
})

/** A workspace row as the organization API writes it: its payer in server-only metadata. */
const organizationRow = (env: Env, id: string, payer: string | null) =>
  sql(env).run(`insert into organization (id, name, slug, metadata, createdAt) values (?, ?, ?, ?, ?)`, [id, id, id, JSON.stringify(payer === null ? {} : { "norn.billingPayer": payer }), new Date().toISOString()])

test("the cell limit is the workspace row's own: at the limit the insert is refused and writes nothing; under it the payer is recorded in the same write", async () => {
  const env = stage(true)
  const { userId } = await enroll(env, "cells@example.com")
  // Free: one cell, held by the provisioned workspace.
  expect(() => organizationRow(env, "org_over", userId)).toThrow("billing.cells_limit")
  expect(sql(env).query("select count(*) as n from organization where id = 'org_over'").get()).toEqual({ n: 0 })
  expect(sql(env).query("select count(*) as n from billing_workspace where organization_id = 'org_over'").get()).toEqual({ n: 0 })
  // Pro: room for two more, each billed from the row that creates it, before any member exists.
  await webhook(env, "subscription.active", subscription({ external_id: userId }))
  organizationRow(env, "org_two", userId)
  expect(sql(env).query("select payer_id from billing_workspace where organization_id = 'org_two'").get()).toEqual({ payer_id: userId })
  organizationRow(env, "org_three", userId)
  expect(() => organizationRow(env, "org_four", userId)).toThrow("billing.cells_limit")
  expect(await ownedWorkspaces(env.AUTH_DB as unknown as BillingDb, userId)).toBe(3)
  // A workspace naming no payer (sign-up provisioning) is not refused; its owner's membership records the payer (0010).
  organizationRow(env, "org_unnamed", null)
  expect(sql(env).query("select count(*) as n from billing_workspace where organization_id = 'org_unnamed'").get()).toEqual({ n: 0 })
  expect(limitRaised(new Error("D1_ERROR: billing.cells_limit: SQLITE_CONSTRAINT"))).toBe("cells")
  expect(limitRaised(new Error("UNIQUE constraint failed"))).toBeNull()
})

test("a client cannot name another payer: the payer key is server-only and survives updates", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "named@example.com")
  const victim = await enroll(env, "victim@example.com")
  await webhook(env, "subscription.active", subscription({ external_id: userId }))
  const created = await post(env, "/api/auth/organization/create", { name: "Mine", slug: "mine", metadata: { "norn.billingPayer": victim.userId, theme: "dark" } }, { cookie })
  expect(created.status).toBe(200)
  const { id } = (await created.json()) as { id: string }
  expect(sql(env).query("select payer_id from billing_workspace where organization_id = ?").get(id)).toEqual({ payer_id: userId })
  expect((await post(env, "/api/auth/organization/update", { organizationId: id, data: { metadata: { "norn.billingPayer": victim.userId } } }, { cookie })).status).toBe(200)
  const { metadata } = sql(env).query("select metadata from organization where id = ?").get(id) as { metadata: string }
  expect(JSON.parse(metadata)["norn.billingPayer"]).toBe(userId)
})

test("a plan with members: 0 still gives a new workspace its owner: no ownerless workspace is billed", async () => {
  const env = stage(true, { free: { cells: 3, members: 0 }, plans: PLANS.plans })
  const { cookie, userId } = await enroll(env, "solo@example.com")
  const created = await post(env, "/api/auth/organization/create", { name: "Solo", slug: "solo" }, { cookie })
  expect(created.status).toBe(200)
  const { id } = (await created.json()) as { id: string }
  expect(sql(env).query("select userId, role from member where organizationId = ?").all(id)).toEqual([{ userId, role: "owner" }])
  const ownerless = `select count(*) as n from billing_workspace w where not exists (select 1 from member m where m.organizationId = w.organization_id and (',' || replace(m.role, ' ', '') || ',') like '%,owner,%')`
  expect(sql(env).query(ownerless).get()).toEqual({ n: 0 })
  // The creator alone: nobody else joins, and an invitation is refused before it is sent.
  expect(() => join_(env, id, "user_other", "member")).toThrow("billing.members_limit")
  const invite = await post(env, "/api/auth/organization/invite-member", { email: "nope@example.com", role: "member", organizationId: id }, { cookie })
  expect(invite.status).toBe(403)
  expect(((await invite.json()) as { refusal: unknown }).refusal).toMatchObject({ limit: "members", allowed: 0, used: 1 })
})

/** Mark an address verified: only a verified address may accept an invitation. */
const verify = (env: Env, userId: string) => sql(env).run(`update user set emailVerified = 1 where id = ?`, [userId])
/** An invitation written directly: invitations are advisory, so the API's pre-check is not what is under test. */
const invitationRow = (env: Env, id: string, organizationId: string, email: string, inviterId: string, status = "pending") => {
  const future = new Date(Date.now() + 86_400_000).toISOString()
  sql(env).run(`insert into invitation (id, organizationId, email, role, status, expiresAt, createdAt, inviterId) values (?, ?, ?, 'member', ?, ?, ?, ?)`, [id, organizationId, email, status, future, future, inviterId])
}
const members = (env: Env, organizationId: string) => (sql(env).query("select count(*) as n from member where organizationId = ?").get(organizationId) as { n: number }).n
interface Invitee { readonly cookie: string; readonly userId: string; readonly invitationId: string }
/** Invitees with verified addresses, one invitation each, to a workspace. */
async function invitees(env: Env, organizationId: string, inviterId: string, names: readonly string[]): Promise<Invitee[]> {
  return Promise.all(names.map(async (name) => {
    const email = `${name}@example.com`
    invitationRow(env, `inv_${name}`, organizationId, email, inviterId)
    const person = await enroll(env, email)
    verify(env, person.userId)
    return { ...person, invitationId: `inv_${name}` }
  }))
}

test("concurrent acceptances at the limit: exactly the limit joins, the rest are refused and their invitations stay open", async () => {
  const env = stage(true)
  const owner = await enroll(env, "busy@example.com")
  const organizationId = firstWorkspace(env, owner.userId)
  // Free: two members, the owner is one. Three accept at once.
  const people = await invitees(env, organizationId, owner.userId, ["p", "q", "r"])
  const responses = await Promise.all(people.map((person) => post(env, "/api/auth/organization/accept-invitation", { invitationId: person.invitationId }, { cookie: person.cookie })))
  expect(responses.map((response) => response.status).sort()).toEqual([200, 403, 403])
  expect(members(env, organizationId)).toBe(2)
  for (const response of responses.filter((one) => one.status === 403)) {
    const body = (await response.json()) as { code: string; message: string; refusal: unknown }
    expect(body).toMatchObject({ code: "PLAN_LIMIT", refusal: { kind: "plan-limit", limit: "members", plan: "free", allowed: 2, used: 2, upgradeUrl: null, upgradeBy: "workspace-owner" } })
    expect(body.message).toContain("ask the workspace's owner to upgrade")
  }
  // Better Auth reopened the refused invitations; nothing else was written. Once a member leaves, a retry joins.
  const refused = people.filter((_, index) => responses[index]?.status === 403)
  expect(refused.map((person) => (sql(env).query("select status from invitation where id = ?").get(person.invitationId) as { status: string }).status)).toEqual(["pending", "pending"])
  const joined = people.find((_, index) => responses[index]?.status === 200)
  sql(env).run("delete from member where organizationId = ? and userId = ?", [organizationId, joined?.userId ?? ""])
  const retry = refused[0] as Invitee
  expect((await post(env, "/api/auth/organization/accept-invitation", { invitationId: retry.invitationId }, { cookie: retry.cookie })).status).toBe(200)
  expect(members(env, organizationId)).toBe(2)
})

test("canceling or rejecting while others accept cannot put a workspace over its limit: invitations hold nothing", async () => {
  const env = stage(true)
  const owner = await enroll(env, "mixed@example.com")
  const organizationId = firstWorkspace(env, owner.userId)
  for (let round = 0; round < 3; round++) {
    const [a, b, c] = await invitees(env, organizationId, owner.userId, [`a${round}`, `b${round}`, `c${round}`]) as [Invitee, Invitee, Invitee]
    // A cancel of A, B's rejection and three acceptances race; whatever wins, only the free seat is filled.
    await Promise.all([
      post(env, "/api/auth/organization/accept-invitation", { invitationId: a.invitationId }, { cookie: a.cookie }),
      post(env, "/api/auth/organization/cancel-invitation", { invitationId: a.invitationId }, { cookie: owner.cookie }),
      post(env, "/api/auth/organization/accept-invitation", { invitationId: b.invitationId }, { cookie: b.cookie }),
      post(env, "/api/auth/organization/reject-invitation", { invitationId: b.invitationId }, { cookie: b.cookie }),
      post(env, "/api/auth/organization/accept-invitation", { invitationId: c.invitationId }, { cookie: c.cookie }),
    ])
    expect(members(env, organizationId)).toBe(2)
    sql(env).run(`delete from member where organizationId = ? and role <> 'owner'`, [organizationId])
  }
})

test("a downgrade from an unlimited plan counts the members already there: they stay, nobody new joins", async () => {
  const env = stage(true)
  const owner = await enroll(env, "down@example.com")
  const organizationId = firstWorkspace(env, owner.userId)
  await webhook(env, "subscription.active", subscription({ external_id: owner.userId, product_id: "prod_team" }))
  for (let n = 0; n < 4; n++) join_(env, organizationId, `user_team_${n}`, "member")
  expect(members(env, organizationId)).toBe(5)
  await webhook(env, "subscription.canceled", subscription({ external_id: owner.userId, product_id: "prod_team", status: "canceled", modified_at: "2026-11-19T11:00:00Z" }))
  const [late] = await invitees(env, organizationId, owner.userId, ["late5"]) as [Invitee]
  const accept = await post(env, "/api/auth/organization/accept-invitation", { invitationId: late.invitationId }, { cookie: late.cookie })
  expect(accept.status).toBe(403)
  expect(((await accept.json()) as { refusal: unknown }).refusal).toMatchObject({ limit: "members", plan: "free", allowed: 2, used: 5 })
  expect(members(env, organizationId)).toBe(5)
  expect(await workspaceMembers(env.AUTH_DB as unknown as BillingDb, organizationId, true)).toBe(6)
  // Pro (ten) has room again.
  await webhook(env, "subscription.active", subscription({ id: "sub_2", external_id: owner.userId, product_id: "prod_pro", modified_at: "2026-11-19T12:00:00Z" }))
  expect((await post(env, "/api/auth/organization/accept-invitation", { invitationId: late.invitationId }, { cookie: late.cookie })).status).toBe(200)
  expect(members(env, organizationId)).toBe(6)
})

test("the database's limits are the entitlement's, and the plan table follows the deployment", async () => {
  const env = stage(true)
  const db = env.AUTH_DB as unknown as BillingDb
  const { userId } = await enroll(env, "agree@example.com")
  const limitsInDb = () => sql(env).query("select cells, members from billing_limit where subject_id = ?").get(userId)
  const cases: { id: string; product: string; status: string }[][] = [
    [],
    [{ id: "s1", product: "prod_pro", status: "active" }],
    [{ id: "s1", product: "prod_pro", status: "past_due" }],
    [{ id: "s1", product: "prod_pro", status: "canceled" }],
    [{ id: "s1", product: "prod_retired", status: "active" }],
    [{ id: "s1", product: "prod_pro", status: "active" }, { id: "s2", product: "prod_team", status: "active" }],
    [{ id: "s1", product: "prod_team", status: "canceled" }, { id: "s2", product: "prod_pro", status: "past_due" }],
  ]
  for (const subscriptions of cases) {
    sql(env).run("delete from billing_subscription")
    for (const one of subscriptions) sql(env).run(`insert into billing_subscription (id, subject_id, product_id, status, polar_status, version, updated_at) values (?, ?, ?, ?, ?, 1, 1)`, [one.id, userId, one.product, one.status, one.status])
    expect({ subscriptions, limits: limitsInDb() }).toEqual({ subscriptions, limits: (await entitlementOf(db, CONFIG, userId)).limits })
  }
  // The Worker wrote the table at its first request; turning billing off empties it, so nothing is limited.
  expect(sql(env).query("select product_id, cells, members from billing_plan order by product_id").all()).toEqual([
    { product_id: "", cells: 1, members: 2 }, { product_id: "prod_pro", cells: 3, members: 10 }, { product_id: "prod_team", cells: null, members: null },
  ])
  await syncPlans(db, undefined)
  expect(sql(env).query("select count(*) as n from billing_plan").get()).toEqual({ n: 0 })
  expect(limitsInDb()).toEqual({ cells: null, members: null })
  // A changed table replaces the old one's rows.
  await syncPlans(db, { ...CONFIG, table: { free: { cells: 2, members: 4 }, plans: [{ id: "pro", name: "Pro", productId: "prod_pro2", limits: { cells: 9, members: 9 } }] } })
  expect(sql(env).query("select product_id, cells, members from billing_plan order by product_id").all()).toEqual([{ product_id: "", cells: 2, members: 4 }, { product_id: "prod_pro2", cells: 9, members: 9 }])
})

test("a live subscription to a product the plan table no longer names grants no limits, but goes to the portal, never a second checkout", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "old@example.com")
  await webhook(env, "subscription.past_due", subscription({ external_id: userId, product_id: "prod_retired", status: "past_due" }))
  expect(await entitlementOf(env.AUTH_DB as unknown as BillingDb, CONFIG, userId)).toEqual({ billing: "enabled", plan: "free", status: "past_due", limits: PLANS.free })
  polarCalls = []
  const checkout = await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie } })
  expect(checkout.status).toBe(303)
  expect(checkout.headers.get("location")).toBe("https://sandbox.polar.sh/portal/cs_1")
  expect(polarCalls.map((entry) => `${entry.method} ${entry.path}`)).not.toContain("POST /v1/checkouts/")
  expect(((await (await call(env, "/api/billing", { headers: { cookie } })).json()) as { actions: unknown }).actions).toEqual([{ kind: "portal", label: "Manage billing", href: `${ORIGIN}/api/billing/portal` }])
})

test("accepting a withdrawn invitation says what to do, not just that it is missing", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "host@example.com")
  const organizationId = firstWorkspace(env, userId)
  const { id } = (await (await post(env, "/api/auth/organization/invite-member", { email: "late@example.com", role: "member", organizationId }, { cookie })).json()) as { id: string }
  sql(env).run("delete from invitation where id = ?", [id])
  const late = await enroll(env, "late@example.com")
  const accept = await post(env, "/api/auth/organization/accept-invitation", { invitationId: id }, { cookie: late.cookie })
  expect(accept.status).toBe(400)
  const body = (await accept.json()) as { code: string; message: string }
  expect(body.code).toBe("INVITATION_NOT_FOUND")
  expect(body.message).toContain("withdrawn")
})

test("a member-limit refusal sends someone who is not the payer to the workspace's owner, never to a checkout billed to them", async () => {
  const env = stage(true)
  const owner = await enroll(env, "own@example.com")
  const admin = await enroll(env, "adm@example.com")
  const organizationId = firstWorkspace(env, owner.userId)
  join_(env, organizationId, admin.userId, "admin")
  // Owner and admin fill the free plan's two seats.
  const refused = await post(env, "/api/auth/organization/invite-member", { email: "new@example.com", role: "member", organizationId }, { cookie: admin.cookie })
  expect(refused.status).toBe(403)
  const body = await refused.json() as { message: string; refusal: unknown }
  expect(body.refusal).toEqual({ kind: "plan-limit", limit: "members", plan: "free", allowed: 2, used: 2, upgradeUrl: null, upgradeBy: "workspace-owner" })
  expect(body.message).toContain("ask the workspace's owner to upgrade")
})

// ===============================================================================================
// Disabled mode (self-hosting) is unlimited and calls nothing

test("without Polar settings, nothing is billed, limited or called", async () => {
  const env = stage(false)
  const { cookie, userId } = await enroll(env, "sam@example.com")
  for (const name of ["two", "three", "four"]) expect((await post(env, "/api/auth/organization/create", { name, slug: name }, { cookie })).status).toBe(200)
  const { id: organizationId } = sql(env).query("select organizationId as id from member where userId = ? order by createdAt limit 1").get(userId) as { id: string }
  for (const email of ["a@example.com", "b@example.com", "c@example.com"]) expect((await post(env, "/api/auth/organization/invite-member", { email, role: "member", organizationId }, { cookie })).status).toBe(200)
  const document = await (await call(env, "/api/billing", { headers: { cookie } })).json()
  expect(document).toEqual({ entitlement: UNLIMITED, usage: { cells: 4 }, actions: [] })
  expect((await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie } })).status).toBe(404)
  expect((await webhook(env, "subscription.active", subscription({ external_id: userId }))).status).toBe(404)
  expect(sql(env).query("select count(*) as n from billing_subscription").get()).toEqual({ n: 0 })
  // The limit triggers refuse nothing: the plan table they read is empty.
  expect(sql(env).query("select count(*) as n from billing_plan").get()).toEqual({ n: 0 })
  for (let n = 0; n < 5; n++) join_(env, organizationId, `user_many_${n}`, "member")
  expect(sql(env).query("select count(*) as n from member where organizationId = ?").get(organizationId)).toEqual({ n: 6 })
  expect(polarCalls).toEqual([])
})

// ===============================================================================================
// The billing document and links; the client only renders them

test("the billing document is the server's: entitlement, usage and the actions it allows", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "ivy@example.com")
  expect((await call(env, "/api/billing")).status).toBe(401)
  expect(await (await call(env, "/api/billing", { headers: { cookie } })).json() as unknown).toEqual({
    entitlement: { billing: "enabled", plan: "free", status: "none", limits: PLANS.free },
    usage: { cells: 1 },
    actions: [
      { kind: "checkout", label: "Upgrade to Pro", href: `${ORIGIN}/api/billing/checkout?plan=pro` },
      { kind: "checkout", label: "Upgrade to Team", href: `${ORIGIN}/api/billing/checkout?plan=team` },
    ],
  })
  await webhook(env, "subscription.active", subscription({ external_id: userId }))
  expect(((await (await call(env, "/api/billing", { headers: { cookie } })).json()) as { actions: unknown }).actions).toEqual([{ kind: "portal", label: "Manage billing", href: `${ORIGIN}/api/billing/portal` }])
})

test("the plugin's own checkout and portal are refused from outside, whatever the caller sends", async () => {
  const env = stage(true)
  const { cookie } = await enroll(env, "eve@example.com")
  const shaped = { slug: "pro", products: ["prod_team"], allow_trial: true, trial_interval: "year", trial_interval_count: 5, discount_id: "d_1", discount_code: "FREE", metadata: { plan: "team" }, success_url: "https://evil.example/x", return_url: "https://evil.example/y", redirect: false }
  for (const path of ["/api/auth/checkout", "/api/auth/checkout/", "/api/auth/Checkout", "/api/auth/customer/portal"]) {
    const response = await post(env, path, shaped, { cookie })
    expect({ path, status: response.status }).toEqual({ path, status: 404 })
  }
  expect((await call(env, "/api/auth/customer/portal", { headers: { cookie } })).status).toBe(404)
  // The customer reads Studio never calls; with a reference_id the plugin would list a workspace's subscriptions.
  for (const path of ["/api/auth/customer/subscriptions/list?reference_id=org_1", "/api/auth/customer/subscriptions/list/?referenceId=org_1", "/api/auth/customer/state", "/api/auth/customer/orders/list", "/api/auth/customer/benefits/list"]) {
    const response = await call(env, path, { headers: { cookie } })
    expect({ path, status: response.status }).toEqual({ path, status: 404 })
  }
  expect(polarCalls).toEqual([])
  // /api/billing/checkout stays the way in, with the server's fixed body: the configured product, no trial or discount.
  expect((await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie } })).status).toBe(303)
  const [checkout] = polarCalls
  expect(checkout?.body).toMatchObject({ products: ["prod_pro"] })
  for (const field of ["trial_interval", "trial_interval_count", "allow_trial", "discount_id", "metadata"]) expect((checkout?.body as Record<string, unknown>)[field] ?? null).toBeNull()
})

test("another site cannot start a checkout or portal session with the person's cookie", async () => {
  const env = stage(true)
  const { cookie } = await enroll(env, "csrf@example.com")
  for (const site of ["cross-site", "same-site"]) expect((await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie, "sec-fetch-site": site } })).status).toBe(403)
  expect((await call(env, "/api/billing/portal", { headers: { cookie, "sec-fetch-site": "cross-site" } })).status).toBe(403)
  expect(polarCalls).toEqual([])
  // Studio's own links (same-origin) and a typed address (none) work.
  expect((await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie, "sec-fetch-site": "same-origin" } })).status).toBe(303)
  expect((await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie, "sec-fetch-site": "none" } })).status).toBe(303)
  // The document itself is read by the shell's fetch and stays readable.
  expect((await call(env, "/api/billing", { headers: { cookie, "sec-fetch-site": "same-origin" } })).status).toBe(200)
})

test("checkout sends a free person to Polar for the configured product under their own id; a paying person goes to the portal", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "max@example.com")
  expect((await call(env, "/api/billing/checkout?plan=pro")).status).toBe(401)
  expect((await call(env, "/api/billing/checkout?plan=gold", { headers: { cookie } })).status).toBe(404)
  const checkout = await call(env, "/api/billing/checkout?plan=pro", { headers: { cookie } })
  expect(checkout.status).toBe(303)
  expect(checkout.headers.get("location")).toBe("https://sandbox.polar.sh/checkout/chk_1")
  expect(polarCalls.find((entry) => entry.path === "/v1/checkouts/")?.body).toMatchObject({ external_customer_id: userId, products: ["prod_pro"] })
  await webhook(env, "subscription.active", subscription({ external_id: userId }))
  polarCalls = []
  const again = await call(env, "/api/billing/checkout?plan=team", { headers: { cookie } })
  expect(again.headers.get("location")).toBe("https://sandbox.polar.sh/portal/cs_1")
  expect(polarCalls.map((entry) => entry.path)).toEqual(["/v1/customer-sessions/"])
  expect(polarCalls[0]?.body).toMatchObject({ external_customer_id: userId })
})

test("no client code decides billing: the browser bundle imports neither Polar nor the entitlement model", () => {
  const root = join(import.meta.dir, "../../src")
  const files = (readdirSync(root, { recursive: true }) as string[]).filter((path) => /\.(ts|tsx)$/.test(path))
  expect(files.length).toBeGreaterThan(0)
  for (const path of files) {
    const text = readFileSync(join(root, path), "utf8")
    expect({ path, polar: /@polar-sh\//.test(text), model: /worker\/(billing|polar)(\.ts)?["']/.test(text), settings: /POLAR_(PLANS|ACCESS_TOKEN|WEBHOOK_SECRET)/.test(text) }).toEqual({ path, polar: false, model: false, settings: false })
  }
})
