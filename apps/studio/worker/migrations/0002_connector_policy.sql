-- Which gateway tools an organization allows its members to bind and run (#772).
-- Default deny: a tool is allowed only by a row naming it, or `*` for every tool of the app.
-- Credentials never live here; they stay in the gateway.
CREATE TABLE "connector_policy" (
  "org_id" text NOT NULL REFERENCES "organization" ("id") ON DELETE CASCADE,
  "app" text NOT NULL,
  "tool" text NOT NULL,
  "granted_by" text NOT NULL,
  "granted_at" integer NOT NULL,
  PRIMARY KEY ("org_id", "app", "tool")
);
