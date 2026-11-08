-- Tool bindings (#772): which gateway tools an organization lets its people run from Norn, each
-- with a label, fixed input values (JSON {field: {fixed: value}}) and the least role that may run it.
-- Default deny: a tool with no binding cannot be run. Credentials never live here; they stay in the gateway.
CREATE TABLE "connector_binding" (
  "id" text NOT NULL PRIMARY KEY,
  "org_id" text NOT NULL REFERENCES "organization" ("id") ON DELETE CASCADE,
  "app" text NOT NULL,
  "tool" text NOT NULL,
  "label" text NOT NULL,
  "input_mapping" text NOT NULL,
  "run_role" text NOT NULL CHECK ("run_role" IN ('member', 'admin')),
  "created_by" text NOT NULL,
  "created_at" integer NOT NULL,
  "updated_by" text NOT NULL,
  "updated_at" integer NOT NULL
);
CREATE INDEX "connector_binding_org" ON "connector_binding" ("org_id", "app", "tool");
