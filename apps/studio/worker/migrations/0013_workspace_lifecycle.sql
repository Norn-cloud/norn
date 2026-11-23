-- Workspace lifecycle (#1414): reversible workspace archiving (so owners can archive and undo/restore
-- without freeing their billed cell quota, while permanently deleting an organization still removes
-- its billing_workspace row via 0010's trigger) and governed workspace requests for deployments that
-- cannot self-serve provision a cell.

CREATE TABLE "workspace_archive" (
  "organization_id" text NOT NULL PRIMARY KEY REFERENCES "organization" ("id") ON DELETE CASCADE,
  "archived_at" text NOT NULL,
  "archived_by" text NOT NULL REFERENCES "user" ("id") ON DELETE CASCADE
);

CREATE TABLE "workspace_request" (
  "id" text NOT NULL PRIMARY KEY,
  "user_id" text NOT NULL REFERENCES "user" ("id") ON DELETE CASCADE,
  "organization_id" text REFERENCES "organization" ("id") ON DELETE SET NULL,
  "name" text NOT NULL,
  "note" text NOT NULL DEFAULT '',
  "status" text NOT NULL CHECK ("status" IN ('pending', 'approved', 'dismissed')) DEFAULT 'pending',
  "created_at" text NOT NULL,
  "decided_at" text,
  "decided_by" text REFERENCES "user" ("id") ON DELETE SET NULL
);
CREATE INDEX "workspace_request_user_idx" ON "workspace_request" ("user_id", "created_at");
CREATE INDEX "workspace_request_org_idx" ON "workspace_request" ("organization_id", "status");
