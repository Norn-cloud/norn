// `norn codegen --ts` fixture: parcel tracking. Golden: ../tracking.d.ts.
// Shared value types (`iso8601.*`, `core.time.*`) resolve from registry/core.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const ParcelFields = Schema.Struct({
  parcel_id: Schema.String,
  carrier: Schema.Literals(["road", "air", "sea"]),
  weight_kg: Norn.decimal(3),
  pieces: Schema.Int,
  fragile: Schema.Boolean,
  note: Schema.NullOr(Schema.String),
  shipped_on: Norn.ref("iso8601.date"),
  scanned_at: Schema.NullOr(Norn.ref("iso8601.timestamp")),
  transit: Norn.ref("iso8601.duration"),
  registered_by: Schema.String,
  label: Schema.String,
})

export const Parcel = Norn.type({
  name: "tracking.Parcel",
  title: "Parcel",
  version: "1.0.0",
  description: "One tracked parcel.",
  properties: ParcelFields,
  identity: [{ name: "primary", properties: ["parcel_id"] }],
})

export const CrossBorderParcel = Norn.type({
  name: "tracking.CrossBorderParcel",
  title: "Cross-border parcel",
  version: "1.0.0",
  extends: Parcel,
  properties: Schema.Struct({ customs_code: Schema.String }),
})

export const RegisterParcel = Norn.action({
  id: "tracking.parcel.register",
  title: "Register a parcel",
  description: "Starts tracking one parcel.",
  version: "1.0.0",
  scopeRequired: ["orgId", "environmentId"],
  idempotent: true,
  ledgered: true,
  input: Schema.Struct({
    parcel_id: Schema.String,
    carrier: Schema.Literals(["road", "air", "sea"]),
    weight_kg: Norn.decimal(3),
    pieces: Schema.Int,
    fragile: Schema.Boolean,
    note: Schema.NullOr(Schema.String),
    shipped_on: Schema.String,
    scanned_at: Schema.NullOr(Schema.String),
    transit: Schema.String,
    registered_by: Schema.String,
    label: Schema.String,
  }),
  output: Schema.Struct({ parcel_id: Schema.String }),
  // The registrar is whoever calls: the server sets it, a caller never sends it.
  fromCaller: ["registered_by"],
  // Derived when the caller leaves it out.
  derive: [{ field: "label", inputRequired: false, expression: Norn.unsafeZen('input.label ?? input.parcel_id') }],
  writes: [{ type: Parcel, change: "created" }, { type: Parcel, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Parcel, objectIdField: "parcel_id" },
})
