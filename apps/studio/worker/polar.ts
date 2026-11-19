import { checkout, polar, portal, webhooks } from "@polar-sh/better-auth"
import { createPolarCore, type PolarCore } from "@polar-sh/sdk/2026-10"
import { deleteExternalCustomers } from "@polar-sh/sdk/2026-10/services/customers"
import { applySubscription, type BillingConfig, type BillingDb, type PolarSubscription } from "./billing.ts"

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
 * Only signed `subscription.*` events change anything: they update the entitlement record
 * (billing.ts). Nothing a client sends (checkout metadata, reference ids) is read for entitlement.
 */
export interface PolarBilling {
  readonly config: BillingConfig
  /** The entitlement record's database. */
  readonly db: BillingDb
  /** Polar's API client; tests pass one aimed at a mock. */
  readonly client?: PolarCore
  readonly now?: () => number
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
          await applySubscription(db, event.data as unknown as PolarSubscription, billing.now?.() ?? Date.now())
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
