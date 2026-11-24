// Type-level contract of generated maps with the @norn/sdk client: `test/codegen.test.ts`
// type-checks this file and requires zero diagnostics, so each `@ts-expect-error` below must be
// a real compile error (an unused one is itself an error).
import { createIntentKey, createNornClient, type ObjectFields } from "../../../../packages/norn-sdk/src/client.ts"
import type { ActionMap as MlActions, DefinitionsDigest as MlDigest, NornDefinitions as MlDefinitions, ObjectMap as MlObjects } from "./ml.d.ts"
import type { ActionMap as TrackingActions, ObjectMap as TrackingObjects } from "./tracking.d.ts"

const scope = { orgId: "org_a", environmentId: "dev" }
const tracking = createNornClient<TrackingActions>({ scope })
const ml = createNornClient<MlActions>({ scope, definitionsVersion: 3 })
const key = createIntentKey()

const parcel = { parcel_id: "p-1", carrier: "air", weight_kg: "12.500", pieces: 2, fragile: false, note: null } as const
void tracking.call("tracking.parcel.register", parcel, { idempotencyKey: key })
void ml.call("ml.run.record", { run_id: "r-1", model: "m", learning_rate: "0.000300", epochs: 3, stage: "queued", accuracy: null }, { idempotencyKey: key })

// @ts-expect-error An action id the definitions do not declare.
void tracking.call("tracking.parcel.teleport", parcel, { idempotencyKey: key })
// @ts-expect-error Another definition set's action is not this client's.
void tracking.call("ml.run.record", parcel, { idempotencyKey: key })
// @ts-expect-error A field the action's input does not declare.
void tracking.call("tracking.parcel.register", { ...parcel, colour: "red" }, { idempotencyKey: key })
// @ts-expect-error A required field is missing.
void tracking.call("tracking.parcel.register", { parcel_id: "p-1", carrier: "air", weight_kg: "1", pieces: 1, fragile: true }, { idempotencyKey: key })
// @ts-expect-error An exact decimal is text, never a JS number.
void tracking.call("tracking.parcel.register", { ...parcel, weight_kg: 12.5 }, { idempotencyKey: key })
// @ts-expect-error A nullable decimal is still text when present.
void ml.call("ml.run.record", { run_id: "r-1", model: "m", learning_rate: "0.1", epochs: 1, stage: "queued", accuracy: 0.9 }, { idempotencyKey: key })
// @ts-expect-error An enum admits only its declared values.
void ml.call("ml.run.record", { run_id: "r-1", model: "m", learning_rate: "0.1", epochs: 1, stage: "deployed", accuracy: null }, { idempotencyKey: key })
// @ts-expect-error A call names its intent's key; the client never mints or derives one.
void tracking.call("tracking.parcel.register", parcel, {})
// @ts-expect-error An intent is fixed to a declared action too.
void tracking.intent("tracking.parcel.lose", parcel)

const stored: ObjectFields<TrackingObjects, "tracking.Parcel"> = { parcel_id: "p-1", carrier: "sea", weight_kg: "1.000", pieces: 1, fragile: true, note: "ok" }
// @ts-expect-error A stored decimal is text too.
const wrongStored: ObjectFields<MlObjects, "ml.TrainingRun"> = { run_id: "r", model: "m", learning_rate: 0.1, epochs: 1, stage: "queued", accuracy: null }
// @ts-expect-error An object type the definitions do not declare.
type Missing = ObjectFields<TrackingObjects, "tracking.Truck">

// The digest is a literal pin of the typed set, not a `string`.
const pinned: MlDefinitions["definitionsDigest"] extends `sha256:${string}` ? (string extends MlDigest ? false : true) : false = true
void [stored, wrongStored, pinned, null as unknown as Missing]
