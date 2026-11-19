-- Hosted billing (worker/billing.ts): plan limits are enforced where the rows are written. A workspace
-- (organization) or member insert that would pass its payer's limit is refused by the trigger below,
-- inside that one statement, so concurrent requests cannot pass a limit together and nothing over a
-- limit is ever written, reserved or undone later. Invitations hold nothing: only joining counts.
-- Refusals are RAISE(ABORT) with the message 'billing.cells_limit' or 'billing.members_limit', which
-- the Worker turns into its typed PLAN_LIMIT refusal (billing.ts limitRaised).

-- The deployment's plan table (POLAR_PLANS), written by the Worker when it starts (billing.ts
-- syncPlans): one row per Polar product, and the free limits under product id ''. NULL is unlimited.
-- Empty when billing is off, so every limit below is unlimited and no trigger refuses anything.
CREATE TABLE "billing_plan" (
  "product_id" text NOT NULL PRIMARY KEY,
  "cells" integer,
  "members" integer
);

-- Each person's limits: the richest plan among their active or past-due subscriptions to a product the
-- table names (most cells, then most members; NULL is the richest), else the free limits. The same
-- rule as billing.ts entitlementOf, which the tests hold equal.
CREATE VIEW "billing_limit" AS
  SELECT u."id" AS "subject_id",
    CASE WHEN EXISTS (SELECT 1 FROM "billing_subscription" s JOIN "billing_plan" p ON p."product_id" = s."product_id"
        WHERE s."subject_id" = u."id" AND s."status" IN ('active', 'past_due') AND p."product_id" <> '')
      THEN (SELECT p."cells" FROM "billing_subscription" s JOIN "billing_plan" p ON p."product_id" = s."product_id"
        WHERE s."subject_id" = u."id" AND s."status" IN ('active', 'past_due') AND p."product_id" <> ''
        ORDER BY p."cells" IS NULL DESC, p."cells" DESC, p."members" IS NULL DESC, p."members" DESC LIMIT 1)
      ELSE (SELECT "cells" FROM "billing_plan" WHERE "product_id" = '') END AS "cells",
    CASE WHEN EXISTS (SELECT 1 FROM "billing_subscription" s JOIN "billing_plan" p ON p."product_id" = s."product_id"
        WHERE s."subject_id" = u."id" AND s."status" IN ('active', 'past_due') AND p."product_id" <> '')
      THEN (SELECT p."members" FROM "billing_subscription" s JOIN "billing_plan" p ON p."product_id" = s."product_id"
        WHERE s."subject_id" = u."id" AND s."status" IN ('active', 'past_due') AND p."product_id" <> ''
        ORDER BY p."cells" IS NULL DESC, p."cells" DESC, p."members" IS NULL DESC, p."members" DESC LIMIT 1)
      ELSE (SELECT "members" FROM "billing_plan" WHERE "product_id" = '') END AS "members"
  FROM "user" u;

-- A workspace created through the organization API names its payer (the creating user) in its
-- server-only metadata key "norn.billingPayer" (auth.ts): refused when that payer already pays for as
-- many workspaces as their plan allows. The workspace provisioned at sign-up names none and is always
-- created; its owner's membership records the payer (migration 0010).
CREATE TRIGGER "billing_cells_limit" BEFORE INSERT ON "organization"
  WHEN (SELECT count(*) FROM "billing_workspace" WHERE "payer_id" =
      CASE WHEN json_valid(new."metadata") THEN json_extract(new."metadata", '$."norn.billingPayer"') END)
    >= (SELECT "cells" FROM "billing_limit" WHERE "subject_id" =
      CASE WHEN json_valid(new."metadata") THEN json_extract(new."metadata", '$."norn.billingPayer"') END)
BEGIN
  SELECT RAISE(ABORT, 'billing.cells_limit');
END;
-- The workspace is billed to that payer from the row that creates it, before any member exists.
CREATE TRIGGER "billing_workspace_on_organization_insert" AFTER INSERT ON "organization"
  WHEN json_valid(new."metadata") AND json_extract(new."metadata", '$."norn.billingPayer"') IS NOT NULL
BEGIN
  INSERT OR IGNORE INTO "billing_workspace" ("organization_id", "payer_id")
    VALUES (new."id", json_extract(new."metadata", '$."norn.billingPayer"'));
END;

-- A member past the workspace payer's member limit is refused. A workspace always admits its first
-- member (its creator, the owner), so creating a workspace never leaves it without an owner: a plan
-- with "members": 0 means the creator alone, the same as 1. max() of a NULL (unlimited) limit is NULL,
-- which never refuses.
CREATE TRIGGER "billing_members_limit" BEFORE INSERT ON "member"
  WHEN (SELECT count(*) FROM "member" WHERE "organizationId" = new."organizationId")
    >= max(1, (SELECT l."members" FROM "billing_workspace" w JOIN "billing_limit" l ON l."subject_id" = w."payer_id"
      WHERE w."organization_id" = new."organizationId"))
BEGIN
  SELECT RAISE(ABORT, 'billing.members_limit');
END;
