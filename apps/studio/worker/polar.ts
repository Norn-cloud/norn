import { checkout, polar, portal, webhooks } from "@polar-sh/better-auth"
import { createPolarCore, type PolarCore } from "@polar-sh/sdk/2026-10"
import { applySubscription, type BillingConfig, type BillingDb, type PolarSubscription } from "./billing.ts"

/**
 * Polar's official Better Auth plugin, configured from the deployment's billing settings. It serves,
 * under /api/auth: `POST /checkout` (a checkout for a configured plan, signed-in people only),
 * `GET|POST /customer/portal` and `/customer/*` (the person's own Polar customer), and
 * `POST /polar/webhooks` (signature-verified with POLAR_WEBHOOK_SECRET, Standard Webhooks, 5-minute
 * window). Each person is a Polar customer whose external id is their user id, created at sign-up.
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
    createCustomerOnSignUp: true,
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
