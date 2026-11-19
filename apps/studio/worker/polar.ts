import { checkout, polar, portal, webhooks } from "@polar-sh/better-auth"
import { createPolarCore, type PolarCore } from "@polar-sh/sdk/2026-10"
import { deleteExternalCustomers } from "@polar-sh/sdk/2026-10/services/customers"
import { applyLimits, FREE_LIMITS, type BillingConfig, type BillingDb, type Grant, type Limits, type SubscriptionStatus, type WebhookOutcome } from "./billing.ts"

/**
 * Polar's official Better Auth plugin, configured from the deployment's billing settings. It serves,
 * under /api/auth: `POST /checkout` and `GET|POST /customer/portal`, which the Worker refuses from
 * outside (index.ts PLUGIN_BILLING_PATHS) and calls itself only from /api/billing with a fixed body
 * (the plugin would forward a caller's trial, discount, product and redirect fields to Polar);
 * `GET /customer/*` (customer reads, which Studio never calls and the Worker also refuses); and `POST /polar/webhooks`
 * (signature-verified with POLAR_WEBHOOK_SECRET, Standard Webhooks, 5-minute window).
 *
 * A person becomes a Polar customer at their first checkout, under their user id as external id. Never
 * at sign-up: the plugin would link an existing Polar customer with the same email (and no external
 * id) to the new, unverified account, handing a stranger that customer's plan and portal.
 *
 * Only signed `subscription.*` events change anything: each becomes the payer's limits record
 * (billing.ts applyLimits), in one write. Nothing a client sends (checkout metadata, reference ids) is
 * read for entitlement. This module is the only place that knows Polar's shapes.
 */
export interface PolarBilling {
  readonly config: BillingConfig
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

/** The fields of a Polar `subscription.*` webhook payload read here (SDK 2026-10, snake_case). */
export interface PolarSubscription {
  readonly id: string
  readonly status: string
  readonly product_id: string
  readonly created_at: string
  readonly modified_at: string | null
  /** Seat-based products: the seats bought, which is the workspace's member limit. */
  readonly seats?: number | null
  readonly product?: { readonly metadata?: Readonly<Record<string, unknown>> | null } | null
  readonly customer: { readonly external_id?: string | null }
}

/** The product metadata keys that set a plan's limits: a non-negative integer, or "unlimited". */
export const LIMIT_METADATA = { cells: "norn_cells", members: "norn_members" } as const

function metadataLimit(value: unknown): number | null | undefined {
  if (value === "unlimited") return null
  const n = typeof value === "number" ? value : typeof value === "string" && /^[0-9]+$/.test(value.trim()) ? Number(value.trim()) : Number.NaN
  return Number.isSafeInteger(n) && n >= 0 ? n : undefined
}

/**
 * What a subscription grants. A live one: its product's limits from metadata (`norn_cells`,
 * `norn_members`), with a seat-based subscription's seats as the member limit; a product without
 * valid limits grants the free ones (it is still a live subscription, so its payer goes to the portal,
 * never a second checkout). A canceled one grants the free limits. The plan label is the POLAR_PLANS
 * slug for the product, else the product id.
 */
export function grantOf(subscription: PolarSubscription, config: BillingConfig): Grant {
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

/** Record one subscription event as its payer's limits (billing.ts applyLimits): one write, never older than what is held. */
export async function applySubscription(db: BillingDb, config: BillingConfig, subscription: PolarSubscription, now: number): Promise<WebhookOutcome> {
  const payer = subscription.customer.external_id
  // A customer with no external id was not created by this Studio; nothing here can be entitled.
  if (typeof payer !== "string" || payer === "") return "ignored"
  const version = Date.parse(subscription.modified_at ?? subscription.created_at)
  if (!Number.isFinite(version)) return "ignored"
  return applyLimits(db, payer, grantOf(subscription, config), { provider: "polar", ref: subscription.id, version }, now)
}

export const polarClient = (config: BillingConfig): PolarCore =>
  createPolarCore({ accessToken: config.accessToken, environment: config.server, timeout: 10 /* seconds */ })

export function polarPlugin(billing: PolarBilling, baseURL: string) {
  const { config, db } = billing
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
        onPayload: async (event) => {
          if (!event.type.startsWith("subscription.")) return
          await applySubscription(db, config, event.data as unknown as PolarSubscription, billing.now?.() ?? Date.now())
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
