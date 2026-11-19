-- Hosted billing (worker/billing.ts): the entitlement record Polar's signed webhooks write. One row per
-- Polar subscription, keyed by its id; "subject_id" is the Better Auth user id (the Polar customer's
-- external_id). "version" is Polar's modified_at in epoch ms: a row only moves to a newer version, so a
-- replayed or reordered webhook cannot roll a subscription back. Limits are not stored: they come from
-- the deployment's POLAR_PLANS, so a plan-table change applies to everyone at once. Unused (empty) on
-- deployments without Polar.
CREATE TABLE "billing_subscription" (
  "id" text NOT NULL PRIMARY KEY,
  "subject_id" text NOT NULL,
  "product_id" text NOT NULL,
  "status" text NOT NULL CHECK ("status" IN ('active', 'past_due', 'canceled')),
  "polar_status" text NOT NULL,
  "version" integer NOT NULL,
  "updated_at" integer NOT NULL
);
CREATE INDEX "billing_subscription_subject" ON "billing_subscription" ("subject_id");
-- Webhook deliveries already processed (the Standard Webhooks webhook-id), so a redelivery or a replay
-- inside the signature's 5-minute window is answered without being applied again. Pruned after a day.
CREATE TABLE "billing_webhook" (
  "id" text NOT NULL PRIMARY KEY,
  "received_at" integer NOT NULL
);
