-- The dashboard assistant's proposals: each is a commitment the person decides. 'pending' (the cell
-- admitted it), 'refused' (the cell refused it; the verdict holds the diagnostics), 'accepted' (installed
-- as the person, with the version it became) or 'declined'. The cell stays the authority for the dashboard;
-- this row is what was asked, what was proposed, what the checker said and what the call cost.
CREATE TABLE "dashboard_proposal" (
  "id" text NOT NULL PRIMARY KEY,
  "org_id" text NOT NULL,
  "environment_id" text NOT NULL,
  "user_id" text NOT NULL,
  "request" text NOT NULL,
  "target_id" text,
  "target_version" integer,
  "definition" text NOT NULL,
  "verdict" text NOT NULL,
  "state" text NOT NULL CHECK ("state" IN ('pending', 'refused', 'accepted', 'declined')),
  "model" text NOT NULL,
  "installed_id" text,
  "installed_version" integer,
  "created_at" integer NOT NULL,
  "decided_at" integer
);
CREATE INDEX "dashboard_proposal_scope" ON "dashboard_proposal" ("org_id", "environment_id", "user_id", "created_at");
-- Every model call the assistant made, priced from the provider's published rates: the spend cap reads this sum.
CREATE TABLE "agent_spend" (
  "id" text NOT NULL PRIMARY KEY,
  "user_id" text NOT NULL,
  "proposal_id" text,
  "model" text NOT NULL,
  "input_tokens" integer NOT NULL,
  "output_tokens" integer NOT NULL,
  "cost_micro_usd" integer NOT NULL,
  "at" integer NOT NULL
);
