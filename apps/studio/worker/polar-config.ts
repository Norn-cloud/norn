import type { BillingSettings, CheckoutPlan } from "./billing.ts"

/**
 * The Polar adapter's deployment settings (POLAR_*), parsed and checked. Only the adapter (polar.ts)
 * and the deploy configs read them; entitlement and admission (billing.ts) take the provider-neutral
 * BillingSettings that `settingsOf` derives. This module imports no SDK, so deploy scripts can use it.
 */

export type PolarServer = "sandbox" | "production"
/** A plan Studio offers at checkout, and the Polar product it is sold as. Its limits come from that product's metadata (polar.ts). */
export interface PolarPlan extends CheckoutPlan {
  readonly productId: string
}
export interface PlanTable {
  readonly plans: readonly PolarPlan[]
}
export interface PolarConfig {
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

/** The product metadata keys that set a plan's limits (polar.ts grantOf): a non-negative integer, or "unlimited". */
export const LIMIT_METADATA = { cells: "norn_cells", members: "norn_members" } as const

/** The settings that turn billing on; all or none. */
export const BILLING_ENV = ["POLAR_ACCESS_TOKEN", "POLAR_WEBHOOK_SECRET", "POLAR_SERVER", "POLAR_PLANS"] as const
export type BillingEnv = Partial<Record<(typeof BILLING_ENV)[number], string>>

/**
 * The scopes POLAR_ACCESS_TOKEN needs, and what each is for. Reconciling a payer reads their
 * subscriptions; checkout and portal mint sessions; deleting an account deletes its customer.
 */
export const POLAR_SCOPES: ReadonlyArray<readonly [scope: string, use: string]> = [
  ["subscriptions:read", "reconcile a payer's limits from their current subscriptions (polar.ts reconcile)"],
  ["checkouts:write", "start a checkout (/api/billing/checkout)"],
  ["customer_sessions:write", "open the customer portal (/api/billing/portal)"],
  ["customers:write", "delete and anonymize a deleted account's customer"],
]

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
  const plans = record.plans.map((entry: unknown, index): PolarPlan => {
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
 * The deployment's Polar settings, or undefined (billing off) when none of them is set at all. Any one
 * declared, even empty, means billing was meant to be on: then all of them must hold a value, and a
 * missing, empty or malformed one refuses. A half-configured deployment must not run unbilled or bill
 * without limits.
 */
export function polarConfig(env: BillingEnv | Readonly<Record<string, string | undefined>>): PolarConfig | undefined {
  if (BILLING_ENV.every((name) => env[name] === undefined)) return undefined
  const missing = BILLING_ENV.filter((name) => (env[name] ?? "").trim() === "")
  if (missing.length > 0) throw new Error(`${BILLING_ENV.join(", ")} go together, each non-empty; missing or empty: ${missing.join(", ")}`)
  const server = env.POLAR_SERVER
  if (server !== "sandbox" && server !== "production") throw new Error(`POLAR_SERVER is "sandbox" or "production", got ${JSON.stringify(server)}`)
  return { accessToken: env.POLAR_ACCESS_TOKEN as string, webhookSecret: env.POLAR_WEBHOOK_SECRET as string, server, table: parsePlans(env.POLAR_PLANS as string) }
}

/** What entitlement and admission need from the deployment: the checkout plans, by slug and name. Nothing of Polar's. */
export const settingsOf = (config: PolarConfig | undefined): BillingSettings | undefined =>
  config === undefined ? undefined : { plans: config.table.plans.map(({ id, name }) => ({ id, name })) }
