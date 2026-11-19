// The golden-path probe's definitions (golden-path.mjs): readings it loads and asks
// about, and a confirmation it acts on. On a hosted probe cell an `approval`
// definition gates probe.confirmation.add; under `norn dev` nothing gates it.
import { Norn } from "@norn/authoring"
import { Schema } from "effect"

export const Reading = Norn.type({
  name: "probe.Reading", title: "Probe reading", version: "0.1.0",
  // amount is additive, so the cell derives a sum metric the probe can ask about.
  properties: Schema.Struct({ id: Schema.String, region: Schema.String, amount: Norn.decimal(2, { additivity: { kind: "additive" } }), occurred_at: Norn.timestamp("utc") }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const AddReading = Norn.action({
  id: "probe.reading.add", title: "Add probe reading", version: "0.1.0",
  // An action input has no timestamp type yet: the time arrives as RFC 3339 text.
  input: Schema.Struct({ id: Schema.String, region: Schema.String, amount: Norn.decimal(2), occurred_at: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Reading, change: "created" }],
  effect: { kind: "upsert_input_object", type: Reading, objectIdField: "id", writeMode: { kind: "create_only" } },
})

export const Confirmation = Norn.type({
  name: "probe.Confirmation", title: "Probe confirmation", version: "0.1.0",
  properties: Schema.Struct({ id: Schema.String, reading: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Confirm = Norn.action({
  id: "probe.confirmation.add", title: "Confirm probe reading", version: "0.1.0",
  input: Schema.Struct({ id: Schema.String, reading: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Confirmation, change: "created" }],
  effect: { kind: "upsert_input_object", type: Confirmation, objectIdField: "id", writeMode: { kind: "create_only" } },
})
