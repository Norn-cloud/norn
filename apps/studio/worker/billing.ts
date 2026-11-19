/**
 * Hosted billing: what each person's plan allows, decided on the server. A payment provider sells the
 * plans and reports them by signed webhook; its adapter (today Polar, polar.ts) turns each event into
 * the payer's limits (applyLimits). This module keeps that per-payer record and answers "may this
 * person create another hosted cell / add another member". Nothing here or in the record is specific
 * to a provider.
 *
 * Billing is off unless the deployment configures Polar. Off, every entitlement is unlimited and no
 * check refuses anything, so self-hosted Studio behaves exactly as before. The browser never decides:
 * it renders the document `/api/billing` returns and the refusals the server sends.
 *
 * The billed subject is a person (Better Auth user id; Polar's customer `external_id`). Each workspace
 * a person pays for (its creator, recorded at creation: migrations 0010, 0011) is one hosted cell; a
 * billed workspace's member limit comes from its payer's plan.
 *
 * The limits are enforced by the database, in the statement that writes the workspace or the member
 * (migration 0011's triggers), so no request can pass one and nothing is reserved or undone. The checks
 * here only explain a refusal or warn before an invitation is sent.
 */

/** null = unlimited. */
export interface Limits {
  /** Workspaces (each with its hosted cell) one person may pay for. */
  readonly cells: number | null
  /** Members one workspace may hold; its first member, the creator, is always admitted. */
  readonly members: number | null
}
/**
 * A plan Studio offers at checkout. Its limits are not here: they come with the provider's event for
 * the product a person subscribes to (polar.ts), so the provider is the one place they are set.
 */
export interface Plan {
  /** The checkout slug, e.g. `pro`. */
  readonly id: string
  readonly name: string
  /** The Polar product this plan is sold as. */
  readonly productId: string
}
export interface PlanTable {
  readonly plans: readonly Plan[]
}
/** What a person without a paid plan (or whose plan was canceled) gets; migration 0011's billing_allowance holds the same constants. */
export const FREE_LIMITS: Limits = { cells: 1, members: 3 }
export type PolarServer = "sandbox" | "production"
export interface BillingConfig {
  readonly accessToken: string
  readonly webhookSecret: string
  readonly server: PolarServer
  readonly table: PlanTable
}

/**
 * PLACEHOLDER plans: the shape POLAR_PLANS takes, not decided prices. The product ids are not real; an
 * operator replaces them with the ids of products created in Polar.
 */
export const PLACEHOLDER_PLANS: PlanTable = {
  plans: [
    { id: "pro", name: "Pro (placeholder)", productId: "00000000-0000-0000-0000-000000000001" },
    { id: "team", name: "Team (placeholder)", productId: "00000000-0000-0000-0000-000000000002" },
  ],
}

/** The settings that turn billing on; all or none. */
export const BILLING_ENV = ["POLAR_ACCESS_TOKEN", "POLAR_WEBHOOK_SECRET", "POLAR_SERVER", "POLAR_PLANS"] as const
export type BillingEnv = Partial<Record<(typeof BILLING_ENV)[number], string>>

/** Plan ids the entitlement uses for "no paid plan" and "billing off". */
const RESERVED_PLANS = new Set(["free", "unlimited"])
const SLUG = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/

/** Parse and check POLAR_PLANS; anything malformed refuses rather than guessing a limit. */
export function parsePlans(text: string): PlanTable {
  let raw: unknown
  try { raw = JSON.parse(text) } catch { throw new Error("POLAR_PLANS must be JSON") }
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) throw new Error("POLAR_PLANS must be an object with plans")
  const record = raw as Record<string, unknown>
  // Limits moved to the product (polar.ts): a table still carrying them would be silently ignored.
  if ("free" in record) throw new Error(`POLAR_PLANS: "free" is gone; the free limits are fixed (billing.ts FREE_LIMITS) and a plan's come from its Polar product's metadata`)
  if (!Array.isArray(record.plans)) throw new Error("POLAR_PLANS: plans must be an array")
  const plans = record.plans.map((entry: unknown, index): Plan => {
    const plan = (entry ?? {}) as Record<string, unknown>
    if (typeof plan.id !== "string" || !SLUG.test(plan.id)) throw new Error(`POLAR_PLANS: plans[${index}].id must be a lowercase slug`)
    if (RESERVED_PLANS.has(plan.id)) throw new Error(`POLAR_PLANS: plans[${index}].id "${plan.id}" is reserved`)
    if (typeof plan.productId !== "string" || plan.productId.trim() === "") throw new Error(`POLAR_PLANS: plans[${index}].productId must be a Polar product id`)
    if ("limits" in plan) throw new Error(`POLAR_PLANS: plans[${index}].limits is gone; set norn_cells and norn_members in the Polar product's metadata`)
    const name = typeof plan.name === "string" && plan.name.trim() !== "" ? plan.name : plan.id
    return { id: plan.id, name, productId: plan.productId }
  })
  if (new Set(plans.map((plan) => plan.id)).size !== plans.length) throw new Error("POLAR_PLANS: plan ids must be unique")
  if (new Set(plans.map((plan) => plan.productId)).size !== plans.length) throw new Error("POLAR_PLANS: product ids must be unique")
  return { plans }
}

/**
 * The deployment's billing, or undefined (billing off) when none of the settings is set at all. Any one
 * declared, even empty, means billing was meant to be on: then all of them must hold a value, and a
 * missing, empty or malformed one refuses. A half-configured deployment must not run unbilled or bill
 * without limits.
 */
export function billingConfig(env: BillingEnv | Readonly<Record<string, string | undefined>>): BillingConfig | undefined {
  if (BILLING_ENV.every((name) => env[name] === undefined)) return undefined
  const missing = BILLING_ENV.filter((name) => (env[name] ?? "").trim() === "")
  if (missing.length > 0) throw new Error(`${BILLING_ENV.join(", ")} go together, each non-empty; missing or empty: ${missing.join(", ")}`)
  const server = env.POLAR_SERVER
  if (server !== "sandbox" && server !== "production") throw new Error(`POLAR_SERVER is "sandbox" or "production", got ${JSON.stringify(server)}`)
  return { accessToken: env.POLAR_ACCESS_TOKEN as string, webhookSecret: env.POLAR_WEBHOOK_SECRET as string, server, table: parsePlans(env.POLAR_PLANS as string) }
}

// ---------------------------------------------------------------------------------------------
// The per-payer limits record (migration 0011): written only by a provider adapter, from signed events.

export type SubscriptionStatus = "active" | "past_due" | "canceled"

/** bun:sqlite (tests, schema scripts) or D1: the same calls. */
type Bound = { all<T>(): Promise<{ results: T[] }>; run(): Promise<unknown> }
type Statement = {
  bind?: (...params: unknown[]) => Bound
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

/** What a provider event grants a payer: the plan's label, its status, and (while it is live) its limits. */
export interface Grant {
  readonly plan: string
  readonly status: SubscriptionStatus
  readonly limits: Limits
}
/** The provider record that decided a grant, and its modification time (epoch ms). */
export interface GrantSource {
  readonly provider: string
  readonly ref: string
  readonly version: number
}

/**
 * Record a payer's limits from one provider event, in one statement. Events may arrive late, twice or
 * out of order: the row only moves to a version at least as new as the one it holds. A payer has one
 * live plan (Studio sends a paying person to the provider's portal, never a second checkout), so the
 * newest event decides, except that ending some other record (a canceled one the row no longer comes
 * from) never takes away the live plan.
 */
export async function applyLimits(db: BillingDb, payerId: string, grant: Grant, source: GrantSource, now: number): Promise<WebhookOutcome> {
  if (payerId === "" || !Number.isSafeInteger(source.version)) return "ignored"
  const written = await rows(db,
    `insert into "billing_limit" ("payer_id", "plan", "status", "cells", "members", "provider", "provider_ref", "version", "updated_at") values (?, ?, ?, ?, ?, ?, ?, ?, ?)
     on conflict ("payer_id") do update set "plan" = excluded."plan", "status" = excluded."status", "cells" = excluded."cells", "members" = excluded."members",
       "provider" = excluded."provider", "provider_ref" = excluded."provider_ref", "version" = excluded."version", "updated_at" = excluded."updated_at"
     where excluded."version" >= "billing_limit"."version"
       and (excluded."status" <> 'canceled' or "billing_limit"."status" = 'canceled'
         or ("billing_limit"."provider" = excluded."provider" and "billing_limit"."provider_ref" = excluded."provider_ref"))
     returning 1 as "applied"`,
    payerId, grant.plan, grant.status, grant.limits.cells, grant.limits.members, source.provider, source.ref, source.version, now)
  return written.length > 0 ? "applied" : "stale"
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

/**
 * A person's entitlement, from their limits record: its plan and limits while active or past due (past
 * due keeps the plan while the provider retries the payment), else the free limits.
 */
export async function entitlementOf(db: BillingDb, billing: BillingConfig | undefined, subjectId: string): Promise<Entitlement> {
  if (billing === undefined) return UNLIMITED
  const [row] = await rows<{ plan: string; status: SubscriptionStatus; cells: number | null; members: number | null }>(db, `select "plan", "status", "cells", "members" from "billing_limit" where "payer_id" = ?`, subjectId)
  if (row === undefined) return { billing: "enabled", plan: "free", status: "none", limits: FREE_LIMITS }
  if (row.status === "canceled") return { billing: "enabled", plan: "free", status: "canceled", limits: FREE_LIMITS }
  return { billing: "enabled", plan: row.plan, status: row.status, limits: { cells: row.cells, members: row.members } }
}

/** The workspaces a person pays for: each is one hosted cell. Counted by the payer recorded at creation, not by today's owner role, so handing the owner role around never frees a cell. */
export async function ownedWorkspaces(db: BillingDb, subjectId: string): Promise<number> {
  const [row] = await rows<{ n: number }>(db, `select count(*) as "n" from "billing_workspace" where "payer_id" = ?`, subjectId)
  return row?.n ?? 0
}

/**
 * A workspace's members, and with `invitations` its pending unexpired invitations too, in one statement
 * so both come from the same snapshot. Invitations are counted only to refuse sending one the workspace
 * could not take; the member limit itself is enforced when a member joins (migration 0011).
 */
export async function workspaceMembers(db: BillingDb, organizationId: string, invitations = false, now = Date.now()): Promise<number> {
  const invited = invitations
    ? ` + (select count(*) from "invitation" where "organizationId" = ? and "status" = 'pending' and (case when typeof("expiresAt") in ('integer', 'real') then "expiresAt" > ? else "expiresAt" > ? end))`
    : ""
  const [row] = await rows<{ n: number }>(db, `select (select count(*) from "member" where "organizationId" = ?)${invited} as "n"`,
    organizationId, ...invitations ? [organizationId, now, new Date(now).toISOString()] : [])
  return row?.n ?? 0
}

/** Who pays for a billed workspace: the payer recorded at its creation (migrations 0010, 0011), or null (not billed, so not limited). */
export async function workspaceOwner(db: BillingDb, organizationId: string): Promise<string | null> {
  const [payer] = await rows<{ payer_id: string }>(db, `select "payer_id" from "billing_workspace" where "organization_id" = ? and "billed" = 1`, organizationId)
  return payer?.payer_id ?? null
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

/**
 * True when the person has a live (active or past-due) subscription, whatever its product: plan changes
 * then go through Polar's portal, never a second checkout.
 */
export const subscribed = (entitlement: Entitlement) => entitlement.billing === "enabled" && (entitlement.status === "active" || entitlement.status === "past_due")

/**
 * Where a person can raise a limit: their customer portal when they already pay (the provider changes
 * the plan there), else a checkout for the first plan in POLAR_PLANS order, else null.
 */
export function upgradeUrl(billing: BillingConfig, baseURL: string, entitlement: Entitlement): string | null {
  if (subscribed(entitlement)) return new URL("/api/billing/portal", baseURL).href
  const plan = billing.table.plans[0]
  return plan === undefined ? null : new URL(`/api/billing/checkout?plan=${encodeURIComponent(plan.id)}`, baseURL).href
}

/** Admitted while `used` is under the payer's limit, else the typed refusal. A check only: the database enforces (migration 0011). */
async function admit(db: BillingDb, billing: BillingConfig, baseURL: string, payerId: string, callerId: string, name: LimitName, used: number): Promise<Admission> {
  const entitlement = await entitlementOf(db, billing, payerId)
  const allowed = entitlement.limits[name]
  if (allowed === null || used < allowed) return { kind: "admitted" }
  // Only the payer can raise their own plan; a link would otherwise bill the caller for someone else's workspace.
  const payer = payerId === callerId
  return { kind: "plan-limit", limit: name, plan: entitlement.plan, allowed, used, upgradeUrl: payer ? upgradeUrl(billing, baseURL, entitlement) : null, upgradeBy: payer ? "you" : "workspace-owner" }
}

/** May this person create another workspace? Unbilled, always. */
export async function admitCell(db: BillingDb, billing: BillingConfig | undefined, baseURL: string, subjectId: string): Promise<Admission> {
  if (billing === undefined) return { kind: "admitted" }
  return admit(db, billing, baseURL, subjectId, subjectId, "cells", await ownedWorkspaces(db, subjectId))
}

/**
 * May this workspace take another member? With `invitations`, its pending invitations count too: that
 * is the check before sending one, so an admin is told before inviting someone who could not join. Its
 * payer's plan decides; a workspace without one is not limited.
 */
export async function admitMember(db: BillingDb, billing: BillingConfig | undefined, baseURL: string, organizationId: string, callerId: string, invitations = false): Promise<Admission> {
  if (billing === undefined) return { kind: "admitted" }
  const payer = await workspaceOwner(db, organizationId)
  if (payer === null) return { kind: "admitted" }
  return admit(db, billing, baseURL, payer, callerId, "members", await workspaceMembers(db, organizationId, invitations))
}

/** The limit a database refusal names (migration 0011's RAISE), or null for any other error. */
export function limitRaised(error: unknown): LimitName | null {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : ""
  if (message.includes("billing.cells_limit")) return "cells"
  if (message.includes("billing.members_limit")) return "members"
  return null
}

export function refusalMessage(refusal: PlanLimitRefusal): string {
  if (refusal.limit === "cells") return `Your ${refusal.plan} plan allows ${refusal.allowed} workspace${refusal.allowed === 1 ? "" : "s"}; upgrade to create another.`
  const allows = `This workspace's ${refusal.plan} plan allows ${refusal.allowed} member${refusal.allowed === 1 ? "" : "s"}`
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
    .map((plan) => ({ kind: "checkout", label: `Upgrade to ${plan.name}`, href: new URL(`/api/billing/checkout?plan=${encodeURIComponent(plan.id)}`, baseURL).href }))
  // Anyone billed once has a Polar customer and can manage it; a person never billed has nothing to manage.
  if (entitlement.status !== "none") actions.push({ kind: "portal", label: "Manage billing", href: new URL("/api/billing/portal", baseURL).href })
  return { entitlement, usage, actions }
}
