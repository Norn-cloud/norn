/**
 * Hosted billing: what each person's plan allows, decided on the server. A payment provider sells the
 * plans; its adapter (today Polar, polar.ts) reads a payer's current state from the provider whenever a
 * signed webhook names them and writes it here whole (writeLimits). This module keeps that per-payer
 * record and answers "may this person create another hosted cell / add another member". Nothing here
 * or in the record is specific to a provider: it takes BillingSettings, never the provider's.
 *
 * Billing is off unless the deployment configures a provider. Off, every entitlement is unlimited, no
 * check refuses anything, and the deploy writes the database's billing mode off (billingModeStatement),
 * so its limit triggers refuse nothing either: self-hosted Studio behaves exactly as before. Requests
 * follow that stored mode (billingOf), never their own settings, so the API and the triggers agree.
 * The browser never decides: it renders the document `/api/billing` returns and the refusals the
 * server sends.
 *
 * The billed subject is a person (Better Auth user id; the provider customer's external id). Each
 * workspace a person pays for (its creator, recorded at creation: migrations 0010, 0011) is one hosted
 * cell; a billed workspace's member limit comes from its payer's plan.
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
/** A plan Studio offers at checkout, by slug and name. Its limits come from the provider (polar.ts). */
export interface CheckoutPlan {
  /** The checkout slug, e.g. `pro`. */
  readonly id: string
  readonly name: string
}
/** Billing as entitlement and admission see it: on (these settings) or off (undefined). Provider-neutral. */
export interface BillingSettings {
  /** The plans offered at checkout, in order; the first is the upgrade a free person is offered. */
  readonly plans: readonly CheckoutPlan[]
}
/** What a person without a paid plan (or whose plan was canceled) gets; migration 0011's billing_allowance holds the same constants. */
export const FREE_LIMITS: Limits = { cells: 1, members: 3 }

// ---------------------------------------------------------------------------------------------
// The per-payer limits record (migration 0011): written only by a provider adapter's reconcile.

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
export type WriteOutcome = "written" | "superseded" | "ignored"

/** A payer's whole current state at the provider: their plan's label, its status, and its limits. */
export interface Grant {
  readonly plan: string
  readonly status: SubscriptionStatus
  readonly limits: Limits
}
/** Where a snapshot came from: the provider, the records it was computed from, and when it was read. */
export interface SnapshotSource {
  readonly provider: string
  /** The provider records that decided it (e.g. subscription ids), for support; empty with none. */
  readonly refs: readonly string[]
  /** When the provider's state was read (epoch ms, taken before the read). */
  readonly asOf: number
}

/**
 * Record a payer's whole state, read from the provider, in one statement. Nothing is derived from a
 * single event, so delivery order, duplicates and replays do not matter: every write is the provider's
 * state as a whole. A snapshot only replaces one read no later than it (`as_of`), so a slow reconcile
 * that read the provider earlier never overwrites a later one; an equal one rewrites the same state.
 * Two reconciles that overlap both write authoritative snapshots, and the later read wins. Any change
 * after a read is followed by its own webhook, whose reconcile reads again, so the row converges on the
 * provider's state.
 */
export async function writeLimits(db: BillingDb, payerId: string, grant: Grant, source: SnapshotSource, now: number): Promise<WriteOutcome> {
  if (payerId === "" || !Number.isSafeInteger(source.asOf)) return "ignored"
  const written = await rows(db,
    `insert into "billing_limit" ("payer_id", "plan", "status", "cells", "members", "provider", "provider_ref", "as_of", "updated_at") values (?, ?, ?, ?, ?, ?, ?, ?, ?)
     on conflict ("payer_id") do update set "plan" = excluded."plan", "status" = excluded."status", "cells" = excluded."cells", "members" = excluded."members",
       "provider" = excluded."provider", "provider_ref" = excluded."provider_ref", "as_of" = excluded."as_of", "updated_at" = excluded."updated_at"
     where excluded."as_of" >= "billing_limit"."as_of"
     returning 1 as "written"`,
    payerId, grant.plan, grant.status, grant.limits.cells, grant.limits.members, source.provider, [...source.refs].sort().join(" "), source.asOf, now)
  return written.length > 0 ? "written" : "superseded"
}

/**
 * The database's billing mode (migration 0011's billing_mode) is the one switch: its limit triggers and
 * every request read it, so the API and the database always apply the same policy. A deploy writes it
 * once, from that deployment's billing settings (alchemy.run.ts, scripts/celld-migrate.ts), in this one
 * statement; no request ever writes it, so isolates of two deployments serving at once cannot flip it.
 */
export const billingModeStatement = (on: boolean) =>
  `INSERT INTO "billing_mode" ("id", "enabled") VALUES (1, ${on ? 1 : 0}) ON CONFLICT ("id") DO UPDATE SET "enabled" = excluded."enabled"`

/** Whether the stored billing mode is on. A database without the row (or without migration 0011) is off, as its triggers are. */
export async function billingModeOn(db: BillingDb): Promise<boolean> {
  try {
    const [mode] = await rows<{ enabled: number }>(db, `select "enabled" from "billing_mode" where "id" = 1`)
    return mode?.enabled === 1
  } catch (error) {
    if (String(error instanceof Error ? error.message : error).includes("no such table: billing_mode")) return false
    throw error
  }
}

/**
 * Billing as a request sees it: on or off as the stored mode says, never as this isolate's settings
 * say. On, the deployment's checkout plans when this isolate has them, else none (limits still apply,
 * nothing is offered at checkout).
 */
export async function billingOf(db: BillingDb, settings: BillingSettings | undefined): Promise<BillingSettings | undefined> {
  return await billingModeOn(db) ? settings ?? { plans: [] } : undefined
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
export async function entitlementOf(db: BillingDb, billing: BillingSettings | undefined, subjectId: string): Promise<Entitlement> {
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
  /** Where the person can raise the limit: a same-origin POST checkout/portal action, else null. */
  readonly upgradeUrl: string | null
  /** HTTP method required to invoke `upgradeUrl` (`"POST"` when `upgradeUrl` is non-null). */
  readonly upgradeMethod?: "POST"
  /** Who can raise it: the caller, or (a member limit refused to someone else) the workspace's payer, whose plan it is; then upgradeUrl is null. */
  readonly upgradeBy: "you" | "workspace-owner"
}
export type Admission = { readonly kind: "admitted" } | PlanLimitRefusal

/**
 * True when the person has a live (active or past-due) subscription, whatever its product: plan changes
 * then go through the provider's portal, never a second checkout.
 */
export const subscribed = (entitlement: Entitlement) => entitlement.billing === "enabled" && (entitlement.status === "active" || entitlement.status === "past_due")

/**
 * Where a person can raise a limit: their customer portal when they already pay (the provider changes
 * the plan there), else a checkout for the first plan offered, else null.
 */
export function upgradeUrl(billing: BillingSettings, baseURL: string, entitlement: Entitlement): string | null {
  if (subscribed(entitlement)) return new URL("/api/billing/portal", baseURL).href
  const plan = billing.plans[0]
  return plan === undefined ? null : new URL(`/api/billing/checkout?plan=${encodeURIComponent(plan.id)}`, baseURL).href
}

/** Admitted while `used` is under the payer's limit, else the typed refusal. A check only: the database enforces (migration 0011). */
async function admit(db: BillingDb, billing: BillingSettings, baseURL: string, payerId: string, callerId: string, name: LimitName, used: number): Promise<Admission> {
  const entitlement = await entitlementOf(db, billing, payerId)
  const allowed = entitlement.limits[name]
  if (allowed === null || used < allowed) return { kind: "admitted" }
  // Only the payer can raise their own plan; a link would otherwise bill the caller for someone else's workspace.
  const payer = payerId === callerId
  const target = payer ? upgradeUrl(billing, baseURL, entitlement) : null
  return {
    kind: "plan-limit",
    limit: name,
    plan: entitlement.plan,
    allowed,
    used,
    upgradeUrl: target,
    ...target === null ? {} : { upgradeMethod: "POST" as const },
    upgradeBy: payer ? "you" : "workspace-owner",
  }
}

/** May this person create another workspace? Unbilled, always. */
export async function admitCell(db: BillingDb, billing: BillingSettings | undefined, baseURL: string, subjectId: string): Promise<Admission> {
  if (billing === undefined) return { kind: "admitted" }
  return admit(db, billing, baseURL, subjectId, subjectId, "cells", await ownedWorkspaces(db, subjectId))
}

/**
 * May this workspace take another member? With `invitations`, its pending invitations count too: that
 * is the check before sending one, so an admin is told before inviting someone who could not join. Its
 * payer's plan decides; a workspace without one is not limited.
 */
export async function admitMember(db: BillingDb, billing: BillingSettings | undefined, baseURL: string, organizationId: string, callerId: string, invitations = false): Promise<Admission> {
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
  /** Billing session creation endpoints accept same-origin POST only and return 303 to Polar. */
  readonly method: "POST"
}
export interface BillingDocument {
  readonly entitlement: Entitlement
  readonly usage: { readonly cells: number }
  readonly actions: readonly BillingAction[]
}

/** Everything the UI shows about billing, decided here; with billing off it has no actions at all. */
export async function billingDocument(db: BillingDb, billing: BillingSettings | undefined, baseURL: string, subjectId: string): Promise<BillingDocument> {
  const entitlement = await entitlementOf(db, billing, subjectId)
  const usage = { cells: await ownedWorkspaces(db, subjectId) }
  if (billing === undefined) return { entitlement, usage, actions: [] }
  // A paying person changes plan in the portal; a second checkout would bill them twice.
  const actions: BillingAction[] = subscribed(entitlement) ? [] : billing.plans
    .map((plan) => ({ kind: "checkout", label: `Upgrade to ${plan.name}`, href: new URL(`/api/billing/checkout?plan=${encodeURIComponent(plan.id)}`, baseURL).href, method: "POST" }))
  // Anyone billed once is the provider's customer and can manage it; a person never billed has nothing to manage.
  if (entitlement.status !== "none") actions.push({ kind: "portal", label: "Manage billing", href: new URL("/api/billing/portal", baseURL).href, method: "POST" })
  return { entitlement, usage, actions }
}
