-- The assistant's spend cap and hourly quota are admitted atomically (#1703). Before any provider call a
-- request reserves its slot and a bounded worst-case cost (every attempt, primary and fallback) in one
-- conditional insert: the cap is checked against settled spend plus every open reservation, and the
-- quota against every reservation the person made in the last hour, paid failures included. The request
-- settles once, by its id, when its usage is known; when usage is uncertain (a provider call threw) it
-- settles at the full reservation, and a request that never settles (a crash after dispatch) keeps its
-- reservation counted.
CREATE TABLE "agent_reservation" (
  "request_id" text NOT NULL PRIMARY KEY,
  "user_id" text NOT NULL,
  "reserved_micro_usd" integer NOT NULL CHECK ("reserved_micro_usd" >= 0),
  "state" text NOT NULL CHECK ("state" IN ('reserved', 'settled')),
  "created_at" integer NOT NULL,
  "settled_at" integer
);
CREATE INDEX "agent_reservation_user" ON "agent_reservation" ("user_id", "created_at");
CREATE INDEX "agent_reservation_open" ON "agent_reservation" ("state");
-- A settled reservation never reopens, and its amount never changes.
CREATE TRIGGER "agent_reservation_final" BEFORE UPDATE ON "agent_reservation"
  WHEN old."state" = 'settled' OR new."reserved_micro_usd" IS NOT old."reserved_micro_usd" OR new."request_id" IS NOT old."request_id" OR new."user_id" IS NOT old."user_id"
BEGIN
  SELECT RAISE(ABORT, 'agent.reservation_final');
END;
