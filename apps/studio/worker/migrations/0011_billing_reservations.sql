-- Hosted billing (worker/billing.ts): plan limits are taken as reservations, each in one conditional
-- write that counts what the workspace or payer already holds in the same statement, so concurrent
-- requests cannot pass a limit together and nothing over the limit is ever written or undone later.
-- Time here is the database's clock (epoch milliseconds from julianday('now')), so a reservation expiring and a reservation being
-- used are ordered by the same writes that decide them.

-- A seat a pending invitation holds, taken before the invitation exists (keyed by the id the
-- invitation is then created with). Live while unexpired; once its invitee accepts it is converted
-- ("accepted_at") and no longer expires, until the member insert consumes it.
CREATE TABLE "billing_seat" (
  "invitation_id" text PRIMARY KEY,
  "organization_id" text NOT NULL,
  "email" text NOT NULL,
  "expires_at" integer NOT NULL,
  "accepted_at" integer
);
CREATE INDEX "billing_seat_organization" ON "billing_seat" ("organization_id");
-- Invitations pending before this migration keep the seat they were admitted with.
INSERT INTO "billing_seat" ("invitation_id", "organization_id", "email", "expires_at")
  SELECT "id", "organizationId", lower("email"),
    CASE WHEN typeof("expiresAt") IN ('integer', 'real') THEN CAST("expiresAt" AS integer)
      ELSE CAST((julianday("expiresAt") - 2440587.5) * 86400000 AS integer) END
  FROM "invitation" WHERE "status" = 'pending';
-- The member an accepted seat was for: the seat becomes that member, in the same write.
CREATE TRIGGER "billing_seat_on_member_insert" AFTER INSERT ON "member"
BEGIN
  DELETE FROM "billing_seat" WHERE "organization_id" = new."organizationId" AND "accepted_at" IS NOT NULL
    AND "email" = (SELECT lower("email") FROM "user" WHERE "id" = new."userId");
END;
-- Canceled or rejected: the seat is free again.
CREATE TRIGGER "billing_seat_on_invitation_closed" AFTER UPDATE OF "status" ON "invitation"
  WHEN new."status" IN ('canceled', 'rejected')
BEGIN
  DELETE FROM "billing_seat" WHERE "invitation_id" = new."id";
END;
-- An acceptance that failed before its member was inserted (Better Auth reopens the invitation): the
-- seat goes back to expiring with its invitation.
CREATE TRIGGER "billing_seat_on_invitation_reopened" AFTER UPDATE OF "status" ON "invitation"
  WHEN old."status" = 'accepted' AND new."status" = 'pending'
BEGIN
  UPDATE "billing_seat" SET "accepted_at" = NULL WHERE "invitation_id" = new."id";
END;
CREATE TRIGGER "billing_seat_on_invitation_delete" AFTER DELETE ON "invitation"
BEGIN
  DELETE FROM "billing_seat" WHERE "invitation_id" = old."id";
END;
CREATE TRIGGER "billing_seat_on_organization_delete" AFTER DELETE ON "organization"
BEGIN
  DELETE FROM "billing_seat" WHERE "organization_id" = old."id";
END;

-- A workspace's cell, reserved before the workspace exists (keyed by the id it is then created with).
-- "reserved_until" is set while only reserved and cleared when the workspace row commits; a
-- reservation that has expired can no longer commit, so an expired one never becomes a workspace
-- after its place was given to another.
ALTER TABLE "billing_workspace" ADD COLUMN "reserved_until" integer;
CREATE TRIGGER "billing_workspace_reservation_expired" BEFORE INSERT ON "organization"
  WHEN EXISTS (SELECT 1 FROM "billing_workspace" WHERE "organization_id" = new."id" AND "reserved_until" IS NOT NULL
    AND "reserved_until" <= CAST((julianday('now') - 2440587.5) * 86400000 AS integer))
BEGIN
  SELECT RAISE(ABORT, 'workspace reservation expired');
END;
CREATE TRIGGER "billing_workspace_on_organization_insert" AFTER INSERT ON "organization"
BEGIN
  UPDATE "billing_workspace" SET "reserved_until" = NULL WHERE "organization_id" = new."id";
END;
