import { Database } from "bun:sqlite"
import { afterEach, beforeEach, expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { getMigrations } from "better-auth/db/migration"
import worker, { type Env } from "../index.ts"
import { authOptions } from "../auth.ts"
import {
  applySubscription, billingConfig, BILLING_ENV, entitlementOf, parsePlans, PLACEHOLDER_PLANS, statusOf, UNLIMITED,
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
function stage(billing: boolean): Env {
  const assets = { fetch: async () => new Response("<!doctype html>studio") } as unknown as Fetcher
  return { ASSETS: assets, AUTH_DB: database() as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip", ...billing ? POLAR_ENV : {} }
}
const sql = (env: Env) => env.AUTH_DB as unknown as Database

// --- the Polar API mock --------------------------------------------------------------------------
interface PolarCall { readonly method: string; readonly path: string; readonly body: unknown }
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
    polarCalls.push({ method: request.method, path: url.pathname, body })
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
  expect(billingConfig({ POLAR_ACCESS_TOKEN: "", POLAR_WEBHOOK_SECRET: "" })).toBeUndefined()
  expect(() => billingConfig({ POLAR_ACCESS_TOKEN: "t" })).toThrow("go together")
  expect(() => billingConfig({ ...POLAR_ENV, POLAR_WEBHOOK_SECRET: "" })).toThrow("POLAR_WEBHOOK_SECRET")
  expect(() => billingConfig({ ...POLAR_ENV, POLAR_SERVER: "live" })).toThrow("POLAR_SERVER")
  expect(CONFIG).toMatchObject({ server: "sandbox", table: PLANS })
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
  expect(["past_due", "unpaid"].map(statusOf)).toEqual(["past_due", "past_due"])
  expect(["canceled", "incomplete", "incomplete_expired", "paused", "something_new"].map(statusOf)).toEqual(Array(5).fill("canceled"))
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
  expect(await entitlementOf(db, CONFIG, "user_2")).toMatchObject({ plan: "free" })
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

test("sign-up creates the person's Polar customer under their user id", async () => {
  const env = stage(true)
  const { userId } = await enroll(env, "cora@example.com")
  const created = polarCalls.find((entry) => entry.method === "POST" && entry.path === "/v1/customers/")
  expect(created?.body).toMatchObject({ email: "cora@example.com", external_id: userId })
})

test("a workspace over the plan's cell limit is refused with a typed refusal and an upgrade link, and admitted after upgrading", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "ada@example.com")
  // Sign-up provisioned one owned workspace: the free plan's one hosted cell.
  const refused = await post(env, "/api/auth/organization/create", { name: "Second", slug: "second" }, { cookie })
  expect(refused.status).toBe(403)
  const body = await refused.json() as { code: string; message: string; refusal: Record<string, unknown> }
  expect(body.code).toBe("PLAN_LIMIT")
  expect(body.refusal).toEqual({ kind: "plan-limit", limit: "cells", plan: "free", allowed: 1, used: 1, upgradeUrl: `${ORIGIN}/api/billing/checkout?plan=pro` })
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
  expect(((await refused.json()) as { refusal: { limit: string } }).refusal).toMatchObject({ kind: "plan-limit", limit: "members", allowed: 2, used: 2 })
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
  expect(polarCalls).toEqual([])
})

// ===============================================================================================
// The billing document and links; the client only renders them

test("the billing document is the server's: entitlement, usage and the actions it allows", async () => {
  const env = stage(true)
  const { cookie, userId } = await enroll(env, "ivy@example.com")
  expect((await call(env, "/api/billing")).status).toBe(401)
  expect(await (await call(env, "/api/billing", { headers: { cookie } })).json()).toEqual({
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
