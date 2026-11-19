import { checkout, polar, portal, webhooks } from "@polar-sh/better-auth"
import { createPolarCore, type PolarCore } from "@polar-sh/sdk/2026-10"
import { deleteExternalCustomers } from "@polar-sh/sdk/2026-10/services/customers"
import { iterListSubscriptions } from "@polar-sh/sdk/2026-10/services/subscriptions"
import { FREE_LIMITS, writeLimits, type BillingDb, type Grant, type Limits, type SubscriptionStatus, type WriteOutcome } from "./billing.ts"
import { LIMIT_METADATA, type PolarConfig } from "./polar-config.ts"

/**
 * Polar's official Better Auth plugin, configured from the deployment's Polar settings (polar-config.ts).
 * It serves, under /api/auth: `POST /checkout` and `GET|POST /customer/portal`, which the Worker refuses
 * from outside (index.ts PLUGIN_BILLING_PATHS) and calls itself only from /api/billing with a fixed body
 * (the plugin would forward a caller's trial, discount, product and redirect fields to Polar);
 * `GET /customer/*` (customer reads, which Studio never calls and the Worker also refuses); and `POST /polar/webhooks`
 * (signature-verified with POLAR_WEBHOOK_SECRET, Standard Webhooks, 5-minute window).
 *
 * A person becomes a Polar customer at their first checkout, under their user id as external id. Never
 * at sign-up: the plugin would link an existing Polar customer with the same email (and no external
 * id) to the new, unverified account, handing a stranger that customer's plan and portal.
 *
 * A signed webhook decides nothing by itself: it only names a payer (the customer's external id), and
 * that payer is reconciled: their live subscriptions are read from Polar and their whole state written
 * (billing.ts writeLimits), in one statement. So delivery order, duplicates and replays cannot change
 * the outcome, and nothing a client sends (checkout metadata, reference ids) is read for entitlement.
 * This module is the only place that knows Polar's shapes.
 */
export interface PolarBilling {
  readonly config: PolarConfig
  /** The entitlement record's database. */
  readonly db: BillingDb
  /** Polar's API client; tests pass one aimed at a mock. */
  readonly client?: PolarCore
  readonly now?: () => number
}

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

/** The fields of a Polar subscription read here (SDK 2026-10, snake_case). */
export interface PolarSubscription {
  readonly id: string
  readonly status: string
  readonly product_id: string
  /** Seat-based products: the seats bought, which is the workspace's member limit. */
  readonly seats?: number | null
  readonly product?: { readonly metadata?: Readonly<Record<string, unknown>> | null } | null
  readonly customer: { readonly external_id?: string | null }
}

function metadataLimit(value: unknown): number | null | undefined {
  if (value === "unlimited") return null
  const n = typeof value === "number" ? value : typeof value === "string" && /^[0-9]+$/.test(value.trim()) ? Number(value.trim()) : Number.NaN
  return Number.isSafeInteger(n) && n >= 0 ? n : undefined
}

/**
 * What one subscription grants. A live one: its product's limits from metadata (`norn_cells`,
 * `norn_members`), with a seat-based subscription's seats as the member limit; a product without
 * valid limits grants the free ones (it is still a live subscription, so its payer goes to the portal,
 * never a second checkout). A canceled one grants the free limits. The plan label is the POLAR_PLANS
 * slug for the product, else the product id.
 */
export function grantOf(subscription: PolarSubscription, config: PolarConfig): Grant {
  const status = statusOf(subscription.status)
  if (status === "canceled") return { plan: "free", status, limits: FREE_LIMITS }
  const plan = config.table.plans.find((candidate) => candidate.productId === subscription.product_id)?.id ?? subscription.product_id
  const metadata = subscription.product?.metadata ?? {}
  const cells = metadataLimit(metadata[LIMIT_METADATA.cells])
  const members = metadataLimit(metadata[LIMIT_METADATA.members])
  const seats = subscription.seats
  const seated = typeof seats === "number" && Number.isSafeInteger(seats) && seats > 0 ? seats : undefined
  if (cells === undefined || (members === undefined && seated === undefined)) return { plan, status, limits: FREE_LIMITS }
  const limits: Limits = { cells, members: seated ?? members ?? null }
  return { plan, status, limits }
}

/** a is at least b, with null (unlimited) above every number. */
const atLeast = (a: number | null, b: number | null) => a === null || (b !== null && a >= b)
const richer = (a: number | null, b: number | null) => atLeast(a, b) ? a : b

/**
 * A payer's whole state from all their subscriptions. Each limit is the richest any live subscription
 * grants; the plan label is the live grant with the most cells, then members, then the lowest
 * subscription id (so the answer never depends on the order Polar lists them); the status is active
 * when any live one is, else past due. Without a live subscription: canceled, the free limits.
 */
export function snapshotOf(subscriptions: readonly PolarSubscription[], config: PolarConfig): Grant {
  const live = subscriptions
    .map((subscription) => ({ id: subscription.id, grant: grantOf(subscription, config) }))
    .filter(({ grant }) => grant.status !== "canceled")
    .sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  if (live.length === 0) return { plan: "free", status: "canceled", limits: FREE_LIMITS }
  const best = live.reduce((top, next) => {
    const t = top.grant.limits, n = next.grant.limits
    const more = t.cells !== n.cells ? !atLeast(t.cells, n.cells) : t.members !== n.members && !atLeast(t.members, n.members)
    return more ? next : top
  })
  return {
    plan: best.grant.plan,
    status: live.some(({ grant }) => grant.status === "active") ? "active" : "past_due",
    limits: live.reduce<Limits>((sum, { grant }) => ({ cells: richer(sum.cells, grant.limits.cells), members: richer(sum.members, grant.limits.members) }), { cells: 0, members: 0 }),
  }
}

/** The Polar statuses that can still grant something; statusOf decides the rest. */
const LIVE_STATUSES = ["active", "trialing", "past_due"] as const

/**
 * Reconcile one payer: read their live subscriptions from Polar (token scope `subscriptions:read`)
 * and write the whole snapshot (billing.ts writeLimits). Idempotent and independent of which event
 * asked for it. A failed read throws, so the webhook answers 5xx and Polar delivers it again.
 */
export async function reconcile(billing: PolarBilling, payerId: string): Promise<WriteOutcome> {
  if (payerId === "") return "ignored"
  const now = billing.now ?? Date.now
  // Taken before the read: the snapshot reflects at least everything Polar held by then.
  const asOf = now()
  const subscriptions: PolarSubscription[] = []
  const list = iterListSubscriptions(billing.client ?? polarClient(billing.config))
  for await (const subscription of list({ external_customer_id: payerId, status: [...LIVE_STATUSES], limit: 100 })) {
    // Only this payer's: a filter Polar ignored must not hand someone else's plan over.
    if (subscription.customer.external_id === payerId) subscriptions.push(subscription)
  }
  return writeLimits(billing.db, payerId, snapshotOf(subscriptions, billing.config), { provider: "polar", refs: snapshotRefs(subscriptions), asOf }, now())
}
const snapshotRefs = (subscriptions: readonly PolarSubscription[]) => subscriptions.filter((subscription) => statusOf(subscription.status) !== "canceled").map((subscription) => subscription.id)

/**
 * The payer a verified webhook names: a subscription's customer, or the customer itself. Anything else
 * (and a customer without an external id, which this Studio did not create) names nobody.
 */
export function payerOf(event: { readonly type: string; readonly data: unknown }): string | null {
  const data = (event.data ?? {}) as { customer?: { external_id?: unknown } | null; external_id?: unknown }
  const id = event.type.startsWith("subscription.") ? data.customer?.external_id : event.type.startsWith("customer.") ? data.external_id : undefined
  return typeof id === "string" && id !== "" ? id : null
}

export const polarClient = (config: PolarConfig): PolarCore =>
  createPolarCore({ accessToken: config.accessToken, environment: config.server, timeout: 10 /* seconds */ })

export function polarPlugin(billing: PolarBilling, baseURL: string) {
  const { config } = billing
  return polar({
    client: billing.client ?? polarClient(config),
    createCustomerOnSignUp: false,
    use: [
      checkout({
        products: config.table.plans.map((plan) => ({ productId: plan.productId, slug: plan.id })),
        successUrl: "/?billing=success&checkout_id={CHECKOUT_ID}",
        returnUrl: baseURL,
        authenticatedUsersOnly: true,
      }),
      portal({ returnUrl: baseURL }),
      webhooks({
        secret: config.webhookSecret,
        // Verified (signature and window) before this runs; the event only says whom to reconcile.
        onPayload: async (event) => {
          const payer = payerOf(event)
          if (payer !== null) await reconcile(billing, payer)
        },
      }),
    ],
  })
}

/**
 * After an account is deleted: delete its Polar customer (cancelling what it pays for), if it ever
 * checked out, and anonymize it so the person's email and name do not stay in Polar. Best effort: a
 * failure is logged (no user data) and never blocks the deletion.
 */
export const forgetPolarCustomer = (billing: PolarBilling) => async (user: { id: string }): Promise<void> => {
  try {
    await deleteExternalCustomers(billing.client ?? polarClient(billing.config))(user.id, { anonymize: true })
  } catch (error) {
    // A person who never checked out has no Polar customer: nothing to delete.
    if ((error as { statusCode?: number } | null)?.statusCode === 404) return
    console.error("Polar customer delete failed", error instanceof Error ? error.name : "unknown error")
  }
}
