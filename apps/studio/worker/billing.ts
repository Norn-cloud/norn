/**
 * Hosted billing: what each person's plan allows, decided on the server. Polar (polar.ts) sells the
 * plans and reports subscriptions by signed webhook; this module keeps the entitlement record those
 * webhooks write and answers "may this person create another hosted cell / add another member".
 *
 * Billing is off unless the deployment configures Polar. Off, every entitlement is unlimited and no
 * check refuses anything, so self-hosted Studio behaves exactly as before. The browser never decides:
 * it renders the document `/api/billing` returns and the refusals the server sends.
 *
 * The billed subject is a person (Better Auth user id = Polar customer `external_id`). Each workspace
 * a person pays for (its first owner, recorded at creation: migration 0010) is one hosted cell; a
 * workspace's member limit comes from its payer's plan.
 */

/** null = unlimited. */
export interface Limits {
  /** Workspaces (each with its hosted cell) one person may pay for. */
  readonly cells: number | null
  /** Members (plus pending invitations) one workspace may hold. */
  readonly members: number | null
}
export interface Plan {
  /** The checkout slug, e.g. `pro`. */
  readonly id: string
  readonly name: string
  /** The Polar product this plan is sold as. */
  readonly productId: string
  readonly limits: Limits
}
export interface PlanTable {
  /** What a person without a paid plan gets. */
  readonly free: Limits
  readonly plans: readonly Plan[]
}
export type PolarServer = "sandbox" | "production"
export interface BillingConfig {
  readonly accessToken: string
  readonly webhookSecret: string
  readonly server: PolarServer
  readonly table: PlanTable
}

/**
 * PLACEHOLDER plans: the shape POLAR_PLANS takes, not decided prices or limits. The product ids are
 * not real; an operator replaces them with the ids of products created in Polar.
 */
export const PLACEHOLDER_PLANS: PlanTable = {
  free: { cells: 1, members: 3 },
  plans: [
    { id: "pro", name: "Pro (placeholder)", productId: "00000000-0000-0000-0000-000000000001", limits: { cells: 5, members: 25 } },
    { id: "team", name: "Team (placeholder)", productId: "00000000-0000-0000-0000-000000000002", limits: { cells: 20, members: null } },
  ],
}

/** The settings that turn billing on; all or none. */
export const BILLING_ENV = ["POLAR_ACCESS_TOKEN", "POLAR_WEBHOOK_SECRET", "POLAR_SERVER", "POLAR_PLANS"] as const
export type BillingEnv = Partial<Record<(typeof BILLING_ENV)[number], string>>

/** Plan ids the entitlement uses for "no paid plan" and "billing off". */
const RESERVED_PLANS = new Set(["free", "unlimited"])
const SLUG = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

function limit(value: unknown, where: string): number | null {
  if (value === null) return null
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) return value
  throw new Error(`POLAR_PLANS: ${where} must be a non-negative integer or null (unlimited)`)
}
function limits(value: unknown, where: string): Limits {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`POLAR_PLANS: ${where} must be an object`)
  const record = value as Record<string, unknown>
  return { cells: limit(record.cells, `${where}.cells`), members: limit(record.members, `${where}.members`) }
}

/** Parse and check POLAR_PLANS; anything malformed refuses rather than guessing a limit. */
export function parsePlans(text: string): PlanTable {
  let raw: unknown
  try { raw = JSON.parse(text) } catch { throw new Error("POLAR_PLANS must be JSON") }
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) throw new Error("POLAR_PLANS must be an object with free and plans")
  const record = raw as Record<string, unknown>
  if (!Array.isArray(record.plans)) throw new Error("POLAR_PLANS: plans must be an array")
  const plans = record.plans.map((entry: unknown, index): Plan => {
    const plan = (entry ?? {}) as Record<string, unknown>
    if (typeof plan.id !== "string" || !SLUG.test(plan.id)) throw new Error(`POLAR_PLANS: plans[${index}].id must be a lowercase slug`)
    if (RESERVED_PLANS.has(plan.id)) throw new Error(`POLAR_PLANS: plans[${index}].id "${plan.id}" is reserved`)
    if (typeof plan.productId !== "string" || plan.productId.trim() === "") throw new Error(`POLAR_PLANS: plans[${index}].productId must be a Polar product id`)
    const name = typeof plan.name === "string" && plan.name.trim() !== "" ? plan.name : plan.id
    return { id: plan.id, name, productId: plan.productId, limits: limits(plan.limits, `plans[${index}].limits`) }
  })
  if (new Set(plans.map((plan) => plan.id)).size !== plans.length) throw new Error("POLAR_PLANS: plan ids must be unique")
  if (new Set(plans.map((plan) => plan.productId)).size !== plans.length) throw new Error("POLAR_PLANS: product ids must be unique")
  return { free: limits(record.free, "free"), plans }
}

/**
 * The deployment's billing, or undefined (billing off) when none of the settings is present. Some but
 * not all of them, or a malformed one, refuses: a half-configured deployment must not run unbilled
 * or bill without limits.
 */
export function billingConfig(env: BillingEnv | Readonly<Record<string, string | undefined>>): BillingConfig | undefined {
  const present = BILLING_ENV.filter((name) => env[name] !== undefined && env[name] !== "")
  if (present.length === 0) return undefined
  if (present.length !== BILLING_ENV.length) throw new Error(`${BILLING_ENV.join(", ")} go together; missing: ${BILLING_ENV.filter((name) => !present.includes(name)).join(", ")}`)
  const server = env.POLAR_SERVER
  if (server !== "sandbox" && server !== "production") throw new Error(`POLAR_SERVER is "sandbox" or "production", got ${JSON.stringify(server)}`)
  return { accessToken: env.POLAR_ACCESS_TOKEN as string, webhookSecret: env.POLAR_WEBHOOK_SECRET as string, server, table: parsePlans(env.POLAR_PLANS as string) }
}

// ---------------------------------------------------------------------------------------------
// The entitlement record (migration 0009_billing): one row per Polar subscription.

export type SubscriptionStatus = "active" | "past_due" | "canceled"

/**
 * Polar's subscription status, as entitlement: trialing counts as active; past due keeps the plan
 * while Polar retries the payment (the only grace); unpaid (retries exhausted) and everything else not
 * being paid for (incomplete, expired, paused, canceled) is canceled. Unknown future statuses are
 * canceled, never granted.
 */
export function statusOf(polarStatus: string): SubscriptionStatus {
  if (polarStatus === "active" || polarStatus === "trialing") return "active"
  if (polarStatus === "past_due") return "past_due"
  return "canceled"
}

/** The fields of a Polar `subscription.*` webhook payload this module reads (SDK v1 snake_case). */
export interface PolarSubscription {
  readonly id: string
  readonly status: string
  readonly product_id: string
  readonly created_at: string
  readonly modified_at: string | null
  readonly customer: { readonly external_id?: string | null }
}

/** bun:sqlite (tests, schema scripts) or D1: the same two calls. */
type Statement = {
  bind?: (...params: unknown[]) => { all<T>(): Promise<{ results: T[] }>; run(): Promise<unknown> }
  all?: (...params: unknown[]) => unknown[]
  run?: (...params: unknown[]) => unknown
}
export type BillingDb = { prepare(sql: string): Statement }

async function rows<T>(db: BillingDb, sql: string, ...params: unknown[]): Promise<T[]> {
  const statement = db.prepare(sql)
  if (statement.bind) return (await statement.bind(...params).all<T>()).results
  return (statement.all?.(...params) ?? []) as T[]
}
async function run(db: BillingDb, sql: string, ...params: unknown[]): Promise<void> {
  const statement = db.prepare(sql)
  if (statement.bind) await statement.bind(...params).run()
  else statement.run?.(...params)
}

export type WebhookOutcome = "applied" | "stale" | "ignored"

/**
 * Record one subscription event. Webhooks may arrive late, twice or out of order: a row only moves to
 * a version at least as new as the one it holds (Polar's `modified_at`), so a replayed or reordered
 * older event can never restore a canceled plan or cancel a renewed one.
 */
export async function applySubscription(db: BillingDb, subscription: PolarSubscription, now: number): Promise<WebhookOutcome> {
  const subject = subscription.customer.external_id
  // A customer with no external id was not created by this Studio; nothing here can be entitled.
  if (typeof subject !== "string" || subject === "") return "ignored"
  const version = Date.parse(subscription.modified_at ?? subscription.created_at)
  if (!Number.isFinite(version)) return "ignored"
  const before = await rows<{ version: number }>(db, `select "version" from "billing_subscription" where "id" = ?`, subscription.id)
  await run(db,
    `insert into "billing_subscription" ("id", "subject_id", "product_id", "status", "polar_status", "version", "updated_at") values (?, ?, ?, ?, ?, ?, ?)
     on conflict ("id") do update set "subject_id" = excluded."subject_id", "product_id" = excluded."product_id", "status" = excluded."status",
       "polar_status" = excluded."polar_status", "version" = excluded."version", "updated_at" = excluded."updated_at"
     where excluded."version" >= "billing_subscription"."version"`,
    subscription.id, subject, subscription.product_id, statusOf(subscription.status), subscription.status, version, now)
  return before[0] !== undefined && before[0].version > version ? "stale" : "applied"
}

/** A webhook delivery id seen before (Polar retries and attackers replay); kept a day, past the signature's 5-minute window. */
export async function webhookSeen(db: BillingDb, id: string): Promise<boolean> {
  return (await rows(db, `select 1 from "billing_webhook" where "id" = ?`, id)).length > 0
}
export async function recordWebhook(db: BillingDb, id: string, now: number): Promise<void> {
  await run(db, `insert into "billing_webhook" ("id", "received_at") values (?, ?) on conflict ("id") do nothing`, id, now)
  await run(db, `delete from "billing_webhook" where "received_at" < ?`, now - 24 * 60 * 60 * 1000)
}

// ---------------------------------------------------------------------------------------------
// Entitlements and admission.

export interface Entitlement {
  readonly billing: "enabled" | "disabled"
  /** The plan id, `free` without one, `unlimited` with billing off. */
  readonly plan: string
  /** The deciding subscription's status; `none` without one. */
  readonly status: SubscriptionStatus | "none"
  readonly limits: Limits
}

export const UNLIMITED: Entitlement = { billing: "disabled", plan: "unlimited", status: "active", limits: { cells: null, members: null } }

const roomy = (value: number | null) => value ?? Number.POSITIVE_INFINITY
const richer = (a: Limits, b: Limits) => roomy(a.cells) - roomy(b.cells) || roomy(a.members) - roomy(b.members)

/**
 * A person's entitlement: the most generous plan among their active or past-due subscriptions (past
 * due keeps the plan while Polar retries the payment; Polar cancels it if that fails), else the free
 * limits. A subscription to a product the plan table does not name grants nothing.
 */
export async function entitlementOf(db: BillingDb, billing: BillingConfig | undefined, subjectId: string): Promise<Entitlement> {
  if (billing === undefined) return UNLIMITED
  const subscriptions = await rows<{ product_id: string; status: SubscriptionStatus }>(db, `select "product_id", "status" from "billing_subscription" where "subject_id" = ? order by "version" desc`, subjectId)
  let best: Entitlement | undefined
  for (const subscription of subscriptions) {
    if (subscription.status === "canceled") continue
    const plan = billing.table.plans.find((candidate) => candidate.productId === subscription.product_id)
    if (plan === undefined) continue
    if (best === undefined || richer(plan.limits, best.limits) > 0) best = { billing: "enabled", plan: plan.id, status: subscription.status, limits: plan.limits }
  }
  if (best !== undefined) return best
  return { billing: "enabled", plan: "free", status: subscriptions.some((row) => row.status === "canceled") ? "canceled" : "none", limits: billing.table.free }
}

/**
 * The workspaces a person pays for: each is one hosted cell. Counted by the payer recorded at creation,
 * not by today's owner role, so handing the owner role around never frees a cell.
 */
export async function ownedWorkspaces(db: BillingDb, subjectId: string): Promise<number> {
  const [row] = await rows<{ n: number }>(db, `select count(*) as "n" from "billing_workspace" where "payer_id" = ?`, subjectId)
  return row?.n ?? 0
}
/** Members and still-pending, unexpired invitations of a workspace. */
export async function workspaceSeats(db: BillingDb, organizationId: string, now = Date.now()): Promise<number> {
  const [members] = await rows<{ n: number }>(db, `select count(*) as "n" from "member" where "organizationId" = ?`, organizationId)
  const [invited] = await rows<{ n: number }>(db, `select count(*) as "n" from "invitation" where "organizationId" = ? and "status" = 'pending' and ${UNEXPIRED}`, organizationId, ...unexpired(now))
  return (members?.n ?? 0) + (invited?.n ?? 0)
}
/**
 * Better Auth writes dates to SQLite/D1 as ISO strings; a row written as epoch milliseconds compares
 * as a number. Either way an invitation whose expiry has passed holds no seat.
 */
const UNEXPIRED = `(case when typeof("expiresAt") in ('integer', 'real') then "expiresAt" > ? else "expiresAt" > ? end)`
const unexpired = (now: number) => [now, new Date(now).toISOString()]

/** Who pays for a workspace: the payer recorded at its creation, else (a workspace from before that record) its earliest owner. */
export async function workspaceOwner(db: BillingDb, organizationId: string): Promise<string | null> {
  const [payer] = await rows<{ payer_id: string }>(db, `select "payer_id" from "billing_workspace" where "organization_id" = ?`, organizationId)
  if (payer !== undefined) return payer.payer_id
  const [row] = await rows<{ userId: string }>(db, `select "userId" from "member" where "organizationId" = ? and (',' || replace("role", ' ', '') || ',') like '%,owner,%' order by "createdAt" asc limit 1`, organizationId)
  return row?.userId ?? null
}

export type LimitName = keyof Limits

/** The typed refusal the server sends when a plan limit is reached; the client renders it as given. */
export interface PlanLimitRefusal {
  readonly kind: "plan-limit"
  readonly limit: LimitName
  readonly plan: string
  readonly allowed: number
  readonly used: number
  /** Where the person can raise the limit: a checkout for the next plan that allows more, else null. */
  readonly upgradeUrl: string | null
  /** Who can raise it: the caller, or (a member limit refused to someone else) the workspace's payer, whose plan it is; then upgradeUrl is null. */
  readonly upgradeBy: "you" | "workspace-owner"
}
export type Admission = { readonly kind: "admitted" } | PlanLimitRefusal

/** True when the person pays for a plan now: plan changes then go through Polar's portal, never a second checkout. */
export const subscribed = (entitlement: Entitlement) => entitlement.billing === "enabled" && entitlement.plan !== "free"

/**
 * Where a person can raise a limit: their customer portal when they already pay (Polar changes the
 * plan there), else a checkout for the first plan (in POLAR_PLANS order) that admits one more, else null.
 */
export function upgradeUrl(billing: BillingConfig, baseURL: string, entitlement: Entitlement, name: LimitName, used: number): string | null {
  if (subscribed(entitlement)) return new URL("/api/billing/portal", baseURL).href
  const plan = billing.table.plans.find((candidate) => roomy(candidate.limits[name]) > used)
  return plan === undefined ? null : new URL(`/api/billing/checkout?plan=${encodeURIComponent(plan.id)}`, baseURL).href
}

/** `used` counts what the new workspace or seat comes after; it is admitted while that is under the limit. */
async function admit(db: BillingDb, billing: BillingConfig | undefined, baseURL: string, payerId: string, callerId: string, name: LimitName, used: () => Promise<number>): Promise<Admission> {
  if (billing === undefined) return { kind: "admitted" }
  const entitlement = await entitlementOf(db, billing, payerId)
  const allowed = entitlement.limits[name]
  if (allowed === null) return { kind: "admitted" }
  const count = await used()
  if (count < allowed) return { kind: "admitted" }
  // Only the payer can raise their own plan; a link would otherwise bill the caller for someone else's workspace.
  const payer = payerId === callerId
  return { kind: "plan-limit", limit: name, plan: entitlement.plan, allowed, used: count, upgradeUrl: payer ? upgradeUrl(billing, baseURL, entitlement, name, count) : null, upgradeBy: payer ? "you" : "workspace-owner" }
}

/** May this person create one more workspace (and its hosted cell)? Every creation path asks this first. */
export const admitCell = (db: BillingDb, billing: BillingConfig | undefined, baseURL: string, subjectId: string) =>
  admit(db, billing, baseURL, subjectId, subjectId, "cells", () => ownedWorkspaces(db, subjectId))

/**
 * After a workspace is created: is it within its payer's limit? Concurrent creations can all pass
 * admitCell before any is written; this recounts with the new row in place. Workspaces are ranked by
 * creation order, so of a racing burst exactly the earliest ones that fit stay and the rest are undone.
 */
export async function admitCreatedCell(db: BillingDb, billing: BillingConfig | undefined, baseURL: string, organizationId: string): Promise<Admission> {
  if (billing === undefined) return { kind: "admitted" }
  const [created] = await rows<{ payer_id: string; seq: number }>(db, `select "payer_id", "seq" from "billing_workspace" where "organization_id" = ?`, organizationId)
  if (created === undefined) return { kind: "admitted" }
  return admit(db, billing, baseURL, created.payer_id, created.payer_id, "cells", async () => {
    const [earlier] = await rows<{ n: number }>(db, `select count(*) as "n" from "billing_workspace" where "payer_id" = ? and "seq" < ?`, created.payer_id, created.seq)
    return earlier?.n ?? 0
  })
}

/** May this workspace take one more member or invitation? Its payer's plan decides; a workspace without one is not limited. */
export async function admitMember(db: BillingDb, billing: BillingConfig | undefined, baseURL: string, organizationId: string, callerId: string): Promise<Admission> {
  if (billing === undefined) return { kind: "admitted" }
  const payer = await workspaceOwner(db, organizationId)
  if (payer === null) return { kind: "admitted" }
  return admit(db, billing, baseURL, payer, callerId, "members", () => workspaceSeats(db, organizationId))
}

/**
 * After an invitation is created: is it within the payer's member limit? Members hold their seats
 * first, then pending invitations in creation order, so of concurrent invitations exactly the earliest
 * ones that fit stay and the rest are undone.
 */
export async function admitCreatedInvitation(db: BillingDb, billing: BillingConfig | undefined, baseURL: string, organizationId: string, invitationId: string, callerId: string, now = Date.now()): Promise<Admission> {
  if (billing === undefined) return { kind: "admitted" }
  const payer = await workspaceOwner(db, organizationId)
  if (payer === null) return { kind: "admitted" }
  return admit(db, billing, baseURL, payer, callerId, "members", async () => {
    const [members] = await rows<{ n: number }>(db, `select count(*) as "n" from "member" where "organizationId" = ?`, organizationId)
    const [earlier] = await rows<{ n: number }>(db,
      `select count(*) as "n" from "invitation" where "organizationId" = ? and "status" = 'pending' and ${UNEXPIRED} and "rowid" < (select "rowid" from "invitation" where "id" = ?)`,
      organizationId, ...unexpired(now), invitationId)
    return (members?.n ?? 0) + (earlier?.n ?? 0)
  })
}

/** Undo a workspace created past its payer's limit (its members, invitations and payer record go with it). */
export async function removeWorkspace(db: BillingDb, organizationId: string): Promise<void> {
  for (const table of ["invitation", "member"]) await run(db, `delete from "${table}" where "organizationId" = ?`, organizationId)
  await run(db, `delete from "organization" where "id" = ?`, organizationId)
  await run(db, `delete from "billing_workspace" where "organization_id" = ?`, organizationId)
}
/** Undo an invitation created past the workspace's member limit. */
export const removeInvitation = (db: BillingDb, invitationId: string) => run(db, `delete from "invitation" where "id" = ?`, invitationId)

export function refusalMessage(refusal: PlanLimitRefusal): string {
  if (refusal.limit === "cells") return `Your ${refusal.plan} plan allows ${refusal.allowed} workspace${refusal.allowed === 1 ? "" : "s"}; upgrade to create another.`
  const allows = `This workspace's ${refusal.plan} plan allows ${refusal.allowed} member${refusal.allowed === 1 ? "" : "s"} (including pending invitations)`
  return refusal.upgradeBy === "you" ? `${allows}; upgrade to add another.` : `${allows}; ask the workspace's owner to upgrade.`
}

// ---------------------------------------------------------------------------------------------
// The billing document the shell renders.

export interface BillingAction {
  readonly kind: "checkout" | "portal"
  readonly label: string
  readonly href: string
}
export interface BillingDocument {
  readonly entitlement: Entitlement
  readonly usage: { readonly cells: number }
  readonly actions: readonly BillingAction[]
}

/** Everything the UI shows about billing, decided here; with billing off it has no actions at all. */
export async function billingDocument(db: BillingDb, billing: BillingConfig | undefined, baseURL: string, subjectId: string): Promise<BillingDocument> {
  const entitlement = await entitlementOf(db, billing, subjectId)
  const usage = { cells: await ownedWorkspaces(db, subjectId) }
  if (billing === undefined) return { entitlement, usage, actions: [] }
  // A paying person changes plan in the portal; a second checkout would bill them twice.
  const actions: BillingAction[] = subscribed(entitlement) ? [] : billing.table.plans
    .filter((plan) => richer(plan.limits, entitlement.limits) > 0)
    .map((plan) => ({ kind: "checkout", label: `Upgrade to ${plan.name}`, href: new URL(`/api/billing/checkout?plan=${encodeURIComponent(plan.id)}`, baseURL).href }))
  // Anyone billed once has a Polar customer and can manage it; a person never billed has nothing to manage.
  if (entitlement.status !== "none") actions.push({ kind: "portal", label: "Manage billing", href: new URL("/api/billing/portal", baseURL).href })
  return { entitlement, usage, actions }
}
