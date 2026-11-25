-- Invite-only sign-up needs an invitation, not just an address (#1703). An operator issues a signed,
-- single-use invitation bound to one email address and an expiry (auth.ts issueSignupInvite); knowing an
-- allowlisted address is no longer enough to bind a password to it. Each invitation is spent at most
-- once: sign-up claims its nonce here (the primary key refuses a second claim) under the new user's id,
-- and the user insert itself consumes the claim, in the same statement's transaction. A claim whose user
-- was never inserted is released by the request that took it, so a refused sign-up can try again; one
-- whose request died is taken over after five minutes.
CREATE TABLE "signup_invite_use" (
  "nonce_hash" text NOT NULL PRIMARY KEY,
  "email" text NOT NULL,
  "user_id" text NOT NULL UNIQUE,
  "claimed_at" integer NOT NULL,
  "consumed_at" integer
);
-- A claim taken over after its request died (auth.ts SIGNUP_CLAIM_STALE_MS) supersedes the old user id:
-- that id can never be inserted, so the stalled request cannot still create a second account.
CREATE TABLE "signup_invite_superseded" ("user_id" text NOT NULL PRIMARY KEY);
CREATE TRIGGER "signup_invite_supersede" AFTER UPDATE OF "user_id" ON "signup_invite_use"
  WHEN old."user_id" IS NOT new."user_id"
BEGIN
  INSERT OR IGNORE INTO "signup_invite_superseded" ("user_id") VALUES (old."user_id");
END;
-- A consumed invitation never changes again.
CREATE TRIGGER "signup_invite_consumed_final" BEFORE UPDATE ON "signup_invite_use"
  WHEN old."consumed_at" IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'signup.invite_consumed');
END;
CREATE TRIGGER "signup_invite_not_superseded" BEFORE INSERT ON "user"
  WHEN EXISTS (SELECT 1 FROM "signup_invite_superseded" WHERE "user_id" = new."id")
BEGIN
  SELECT RAISE(ABORT, 'signup.invite_superseded');
END;
-- A claimed user is inserted only under its own address and only once.
CREATE TRIGGER "signup_invite_claim_matches" BEFORE INSERT ON "user"
  WHEN EXISTS (SELECT 1 FROM "signup_invite_use" WHERE "user_id" = new."id")
BEGIN
  SELECT RAISE(ABORT, 'signup.invite_invalid')
  WHERE NOT EXISTS (SELECT 1 FROM "signup_invite_use" WHERE "user_id" = new."id" AND "email" = lower(new."email") AND "consumed_at" IS NULL);
END;
CREATE TRIGGER "signup_invite_consume" AFTER INSERT ON "user"
  WHEN EXISTS (SELECT 1 FROM "signup_invite_use" WHERE "user_id" = new."id")
BEGIN
  UPDATE "signup_invite_use" SET "consumed_at" = CAST((julianday('now') - 2440587.5) * 86400000 AS integer) WHERE "user_id" = new."id" AND "consumed_at" IS NULL;
END;
