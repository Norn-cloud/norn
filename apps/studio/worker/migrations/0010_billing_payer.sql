-- Hosted billing (worker/billing.ts): who pays for each workspace (its hosted cell). The payer is the
-- workspace's first owner, recorded when that owner's membership is inserted, on every creation path
-- (the organization API and sign-up provisioning alike). Later role changes never move it, so
-- promoting someone to owner and demoting yourself does not free a cell under your plan. "seq" orders
-- creations: when concurrent creations race past the limit, the later ones are undone (billing.ts).
CREATE TABLE "billing_workspace" (
  "seq" integer PRIMARY KEY AUTOINCREMENT,
  "organization_id" text NOT NULL UNIQUE,
  "payer_id" text NOT NULL
);
CREATE INDEX "billing_workspace_payer" ON "billing_workspace" ("payer_id");
-- Existing workspaces: the earliest owner pays, in creation order.
INSERT INTO "billing_workspace" ("organization_id", "payer_id")
  SELECT "id", "payer" FROM (
    SELECT o."id", o."createdAt", (
      SELECT m."userId" FROM "member" m
      WHERE m."organizationId" = o."id" AND (',' || replace(m."role", ' ', '') || ',') LIKE '%,owner,%'
      ORDER BY m."createdAt" ASC, m."id" ASC LIMIT 1
    ) AS "payer" FROM "organization" o
  ) WHERE "payer" IS NOT NULL ORDER BY "createdAt" ASC, "id" ASC;
CREATE TRIGGER "billing_workspace_on_owner_insert" AFTER INSERT ON "member"
  WHEN (',' || replace(new."role", ' ', '') || ',') LIKE '%,owner,%'
BEGIN
  INSERT OR IGNORE INTO "billing_workspace" ("organization_id", "payer_id") VALUES (new."organizationId", new."userId");
END;
CREATE TRIGGER "billing_workspace_on_organization_delete" AFTER DELETE ON "organization"
BEGIN
  DELETE FROM "billing_workspace" WHERE "organization_id" = old."id";
END;
