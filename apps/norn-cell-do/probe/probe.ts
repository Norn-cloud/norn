// The golden-path probe's definitions (golden-path.mjs): readings it loads and asks
// about, a confirmation it acts on, and a sentinel whose duplicate create it
// expects refused. Every object sits in a fixed slot and is upserted with the
// run's id (PROBE-BOUNDED-1), so a probe cell never grows past five objects. On a
// hosted probe cell an `approval` definition gates probe.confirmation.put; under
// `norn dev` nothing gates it.
import { Norn } from "@norn/authoring"
import { Schema } from "effect"

export const Reading = Norn.type({
  name: "probe.Reading", title: "Probe reading", version: "0.2.0",
  // amount is additive, so the cell derives a sum metric the probe can ask about; run ties a row to its run.
  properties: Schema.Struct({ id: Schema.String, run: Schema.String, region: Schema.String, amount: Norn.decimal(2, { additivity: { kind: "additive" } }), occurred_at: Norn.timestamp("utc") }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const PutReading = Norn.action({
  id: "probe.reading.put", title: "Put probe reading", version: "0.2.0",
  // An action input has no timestamp type yet: the time arrives as RFC 3339 text.
  input: Schema.Struct({ id: Schema.String, run: Schema.String, region: Schema.String, amount: Norn.decimal(2), occurred_at: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Reading, change: "created" }, { type: Reading, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Reading, objectIdField: "id", writeMode: { kind: "upsert" } },
})

export const Confirmation = Norn.type({
  name: "probe.Confirmation", title: "Probe confirmation", version: "0.2.0",
  properties: Schema.Struct({ id: Schema.String, reading: Schema.String, run: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Confirm = Norn.action({
  id: "probe.confirmation.put", title: "Confirm probe reading", version: "0.2.0",
  input: Schema.Struct({ id: Schema.String, reading: Schema.String, run: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Confirmation, change: "created" }, { type: Confirmation, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Confirmation, objectIdField: "id", writeMode: { kind: "upsert" } },
})

export const Sentinel = Norn.type({
  name: "probe.Sentinel", title: "Probe sentinel", version: "0.2.0",
  properties: Schema.Struct({ id: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const AddSentinel = Norn.action({
  id: "probe.sentinel.add", title: "Add probe sentinel", version: "0.2.0",
  input: Schema.Struct({ id: Schema.String }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Sentinel, change: "created" }],
  effect: { kind: "upsert_input_object", type: Sentinel, objectIdField: "id", writeMode: { kind: "create_only" } },
})
