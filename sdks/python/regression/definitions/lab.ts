import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// The demo's dataset: seeded samples with known violations (synthetic.py).
const SampleFields = Schema.Struct({
  sample: Schema.String,
  t: Schema.Int,
  x1: Norn.decimal(4),
  x2: Norn.decimal(4),
  x3: Norn.decimal(4),
  y: Norn.decimal(4),
})

export const Sample = Norn.type({
  name: "lab.Sample", title: "Lab sample", version: "0.1.0",
  properties: SampleFields,
  identity: [{ name: "primary", properties: ["sample"] }],
})

export const AddSample = Norn.action({
  id: "lab.sample.add", title: "Add a lab sample", version: "0.1.0",
  input: SampleFields,
  output: Schema.Struct({ sample: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: Sample, change: "created" }],
  effect: { kind: "upsert_input_object", type: Sample, objectIdField: "sample", writeMode: { kind: "create_only" } },
})
