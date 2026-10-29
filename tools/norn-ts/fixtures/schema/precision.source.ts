// T7-EFX-4 (#2787) numeric precision round trip. Golden: precision.ir.json.
// Defaults keep their exact source text: 28 fraction digits, an integer above
// 2^53, and trailing zeros that carry the scale. None of them could survive a
// binary float.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

class Reading extends Schema.Class<Reading>("Reading")({
  reading_id: Schema.String,
  ratio: Norn.decimal(28),
  counter: Schema.Int,
  price: Norn.decimal(4),
  offset: Norn.decimal(1),
  note: Schema.NullOr(Schema.String),
}) {}

// Shorthand metadata members read their bindings.
const version = "0.1.0"

export const Meter = Norn.type({
  name: "metering.Meter",
  title: "Meter",
  version,
  properties: Reading,
  identity: [{ name: "primary", properties: ["reading_id"] }],
  defaults: { ratio: 0.1000000000000000000000000001, counter: 9007199254740993, price: 12.5000, offset: -0.5 },
})
