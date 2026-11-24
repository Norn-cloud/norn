// Type-level contract of generated maps with the @norn/sdk client: `test/codegen.test.ts`
// type-checks this file and requires zero diagnostics, so each `@ts-expect-error` below must be
// a real compile error (an unused one is itself an error).
import { createIntentKey, createNornClient, type ObjectFields } from "../../../../packages/norn-sdk/src/index.ts"
import type { ActionMap as MlActions, DefinitionsDigest as MlDigest, NornDefinitions as MlDefinitions, ObjectMap as MlObjects } from "./ml.d.ts"
import type { ActionMap as TrackingActions, ObjectMap as TrackingObjects } from "./tracking.d.ts"

const scope = { orgId: "org_a", environmentId: "dev" }
const tracking = createNornClient<TrackingActions>({ scope })
const ml = createNornClient<MlActions>({ scope, definitionsVersion: 3 })
const key = createIntentKey()

const parcel = {
  parcel_id: "p-1", carrier: "air", weight_kg: "12.500", pieces: 2, fragile: false, note: null,
  shipped_on: "2026-11-24", scanned_at: null, transit: "P2D",
} as const
const run = { run_id: "r-1", model: "m", learning_rate: "0.000300", epochs: 3, accuracy: null } as const
const start = { run_id: "r-1", tags: ["baseline"], budget: { max_epochs: 10, max_cost: "25.00" } } as const

// What compiles: the derived `label` may be left out or sent; an optional member may be absent.
void tracking.call("tracking.parcel.register", parcel, { idempotencyKey: key })
void tracking.call("tracking.parcel.register", { ...parcel, label: "fragile glass" }, { idempotencyKey: key })
void ml.call("ml.run.record", run, { idempotencyKey: key })
void ml.call("ml.run.start", start, { idempotencyKey: key, definitionsVersion: 4 })
void ml.pinned(5).call("ml.run.start", { ...start, reason: "rerun" }, { idempotencyKey: key })

// @ts-expect-error An action id the definitions do not declare.
void tracking.call("tracking.parcel.teleport", parcel, { idempotencyKey: key })
// @ts-expect-error Another definition set's action is not this client's.
void tracking.call("ml.run.record", parcel, { idempotencyKey: key })
// @ts-expect-error A field the action's input does not declare.
void tracking.call("tracking.parcel.register", { ...parcel, colour: "red" }, { idempotencyKey: key })
// @ts-expect-error ACTION-ACTOR-1: `registered_by` is derived from the verified caller, never sent.
void tracking.call("tracking.parcel.register", { ...parcel, registered_by: "someone-else" }, { idempotencyKey: key })
// @ts-expect-error A required field is missing.
void tracking.call("tracking.parcel.register", { parcel_id: "p-1", carrier: "air", weight_kg: "1", pieces: 1, fragile: true, note: null, scanned_at: null, transit: "P1D" }, { idempotencyKey: key })
// @ts-expect-error An exact decimal is text, never a JS number.
void tracking.call("tracking.parcel.register", { ...parcel, weight_kg: 12.5 }, { idempotencyKey: key })
// @ts-expect-error A nullable decimal is still text when present.
void ml.call("ml.run.record", { ...run, accuracy: 0.9 }, { idempotencyKey: key })
// @ts-expect-error A decimal nested in a record is text too.
void ml.call("ml.run.start", { ...start, budget: { max_epochs: 10, max_cost: 25 } }, { idempotencyKey: key })
// @ts-expect-error A list holds only its declared item type.
void ml.call("ml.run.start", { ...start, tags: [1, 2] }, { idempotencyKey: key })
// @ts-expect-error An enum admits only its declared values.
void tracking.call("tracking.parcel.register", { ...parcel, carrier: "rail" }, { idempotencyKey: key })
// @ts-expect-error A call names its intent's key; the client never mints or derives one.
void tracking.call("tracking.parcel.register", parcel, {})
// @ts-expect-error An intent is fixed to a declared action too.
void tracking.intent("tracking.parcel.lose", parcel)

const stored: ObjectFields<TrackingObjects, "tracking.CrossBorderParcel"> = {
  parcel_id: "p-1", carrier: "sea", weight_kg: "1.000", pieces: 1, fragile: true, note: "ok", shipped_on: "2026-11-24",
  scanned_at: "2026-11-24T10:00:00Z", transit: "PT4H", registered_by: "user-1", label: "p-1", customs_code: "HS-1",
}
// @ts-expect-error A subtype carries its supertype's fields: `customs_code` alone is not a stored parcel.
const partial: ObjectFields<TrackingObjects, "tracking.CrossBorderParcel"> = { customs_code: "HS-1" }
// @ts-expect-error A stored decimal is text too.
const wrongStored: ObjectFields<MlObjects, "ml.TrainingRun"> = { run_id: "r", model: "m", learning_rate: 0.1, epochs: 1, stage: "queued", accuracy: null, window: null }
// @ts-expect-error A stored record holds its members, not free text.
const wrongWindow: ObjectFields<MlObjects, "ml.TrainingRun"> = { run_id: "r", model: "m", learning_rate: "0.1", epochs: 1, stage: "queued", accuracy: null, window: "2026" }
// @ts-expect-error An object type the definitions do not declare.
type Missing = ObjectFields<TrackingObjects, "tracking.Truck">

// The digest is a literal pin of the typed set, not a `string`.
const pinned: MlDefinitions["definitionsDigest"] extends `sha256:${string}` ? (string extends MlDigest ? false : true) : false = true
void [stored, partial, wrongStored, wrongWindow, pinned, null as unknown as Missing]
