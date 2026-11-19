-- Hosted billing (worker/billing.ts): plan limits are enforced where the rows are written. A workspace
-- (organization) or member insert that would pass its payer's limit is refused by the triggers below,
-- inside that one statement, so concurrent requests cannot pass a limit together and nothing over a
-- limit is ever written, reserved or undone later. Invitations hold nothing: only joining counts.
-- Refusals are RAISE(ABORT) with the message 'billing.cells_limit' or 'billing.members_limit', which
-- the Worker turns into its typed PLAN_LIMIT refusal (billing.ts limitRaised).
--
-- Nothing here names a payment provider. A provider's adapter (today polar.ts) reads a payer's whole
-- current state from the provider when a signed webhook names them, and writes it as one row per payer
-- (billing.ts writeLimits); the triggers read only that row and the billing mode.

-- Each payer's limits: a snapshot of their whole state at the provider, written in one statement. NULL
-- is unlimited. "status" is the paid plan's: a canceled one grants the free limits. "provider" and
-- "provider_ref" say where the snapshot came from (e.g. the live subscription ids), and "as_of" is when
-- the provider was read, in epoch ms: a row only moves to a snapshot read no earlier, so a slower,
-- older read never replaces a newer one. No single event decides a row, so delivery order and replays
-- do not matter.
CREATE TABLE "billing_limit" (
  "payer_id" text NOT NULL PRIMARY KEY,
  "plan" text NOT NULL,
  "status" text NOT NULL CHECK ("status" IN ('active', 'past_due', 'canceled')),
  "cells" integer CHECK ("cells" IS NULL OR "cells" >= 0),
  "members" integer CHECK ("members" IS NULL OR "members" >= 0),
  "provider" text NOT NULL,
  "provider_ref" text NOT NULL,
  "as_of" integer NOT NULL,
  "updated_at" integer NOT NULL
);

-- Per-subscription records (migration 0009) are replaced by the per-payer row above; no environment
-- ever held any.
DROP INDEX "billing_subscription_subject";
DROP TABLE "billing_subscription";
-- Deliveries no longer need remembering: a webhook only triggers a reconcile, which is idempotent.
DROP TABLE "billing_webhook";

-- Whether billing is on, as the Worker last saw its settings (billing.ts syncBillingMode). Off, no
-- trigger below refuses anything, matching the API, which reports every limit unlimited: turning
-- billing off frees workspaces billed while it was on. Off until a Worker with billing on says so.
CREATE TABLE "billing_mode" (
  "id" integer NOT NULL PRIMARY KEY CHECK ("id" = 1),
  "enabled" integer NOT NULL CHECK ("enabled" IN (0, 1))
);
INSERT INTO "billing_mode" ("id", "enabled") VALUES (1, 0);

-- What each person may use: their row's limits while the plan is active or past due, else the free
-- limits, one workspace and three members (the same constants as billing.ts FREE_LIMITS).
CREATE VIEW "billing_allowance" AS
  SELECT u."id" AS "subject_id",
    CASE WHEN l."status" IN ('active', 'past_due') THEN l."cells" ELSE 1 END AS "cells",
    CASE WHEN l."status" IN ('active', 'past_due') THEN l."members" ELSE 3 END AS "members"
  FROM "user" u LEFT JOIN "billing_limit" l ON l."payer_id" = u."id";

-- Only workspaces created while billing is on are billed: the server marks them (auth.ts, workspaces.ts),
-- so a deployment without billing writes no mark and nothing is ever limited there.
ALTER TABLE "billing_workspace" ADD COLUMN "billed" integer NOT NULL DEFAULT 0;

-- Both limit triggers refuse only while billing is on (billing_mode).
-- A workspace created through Studio's workspace API names its payer (the creating user) in its
-- server-only metadata key "norn.billingPayer" (workspaces.ts): refused when that payer already pays
-- for as many workspaces as their plan allows. The workspace provisioned at sign-up names none and is
-- always created.
CREATE TRIGGER "billing_cells_limit" BEFORE INSERT ON "organization"
  WHEN (SELECT "enabled" FROM "billing_mode" WHERE "id" = 1) = 1
    AND (SELECT count(*) FROM "billing_workspace" WHERE "payer_id" =
      CASE WHEN json_valid(new."metadata") THEN json_extract(new."metadata", '$."norn.billingPayer"') END)
    >= (SELECT "cells" FROM "billing_allowance" WHERE "subject_id" =
      CASE WHEN json_valid(new."metadata") THEN json_extract(new."metadata", '$."norn.billingPayer"') END)
BEGIN
  SELECT RAISE(ABORT, 'billing.cells_limit');
END;
-- The workspace is billed to that payer from the row that creates it, before any member exists.
CREATE TRIGGER "billing_workspace_on_organization_insert" AFTER INSERT ON "organization"
  WHEN json_valid(new."metadata") AND json_extract(new."metadata", '$."norn.billingPayer"') IS NOT NULL
BEGIN
  INSERT OR IGNORE INTO "billing_workspace" ("organization_id", "payer_id", "billed")
    VALUES (new."id", json_extract(new."metadata", '$."norn.billingPayer"'), 1);
END;
-- The first owner of any other workspace pays for it (migration 0010), billed when the server marked
-- the workspace "norn.billed" (sign-up provisioning with billing on).
DROP TRIGGER "billing_workspace_on_owner_insert";
CREATE TRIGGER "billing_workspace_on_owner_insert" AFTER INSERT ON "member"
  WHEN (',' || replace(new."role", ' ', '') || ',') LIKE '%,owner,%'
BEGIN
  INSERT OR IGNORE INTO "billing_workspace" ("organization_id", "payer_id", "billed")
    VALUES (new."organizationId", new."userId", coalesce((SELECT CASE WHEN json_valid(o."metadata") AND json_extract(o."metadata", '$."norn.billed"') = 1 THEN 1 ELSE 0 END
      FROM "organization" o WHERE o."id" = new."organizationId"), 0));
END;

-- A member past a billed workspace's payer's member limit is refused. A workspace always admits its
-- first member (its creator, the owner), so creating a workspace never leaves it without an owner: a
-- plan with "members": 0 means the creator alone, the same as 1. max() of a NULL (unlimited) limit is
-- NULL, which never refuses.
CREATE TRIGGER "billing_members_limit" BEFORE INSERT ON "member"
  WHEN (SELECT "enabled" FROM "billing_mode" WHERE "id" = 1) = 1
    AND (SELECT count(*) FROM "member" WHERE "organizationId" = new."organizationId")
    >= max(1, (SELECT a."members" FROM "billing_workspace" w JOIN "billing_allowance" a ON a."subject_id" = w."payer_id"
      WHERE w."organization_id" = new."organizationId" AND w."billed" = 1))
BEGIN
  SELECT RAISE(ABORT, 'billing.members_limit');
END;
