import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Conversion } from "../../registry/core/src/physics"

const StoredMeasurements = Schema.Struct({
  id: Schema.String,
  input_quantity: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  output_quantity: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  loss_quantity: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
})
const Measurements = Schema.Struct({
  id: Schema.String,
  input_quantity: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  output_quantity: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  loss_quantity: Schema.optionalKey(Norn.decimal(0, { unit: { system: "ucum", code: "g" } })),
})

export const Melt = Norn.type({
  name: "constitution.Melt", title: "Bounded fine-metal conversion", version: "1.0.0",
  role: "happening", implements: [Conversion],
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: StoredMeasurements,
  defaults: { loss_quantity: 0 },
  identity: [{ name: "primary", properties: ["id"] }],
})

export const Conservation = Norn.law({
  id: "constitution.fine_mass.law", title: "Measured input equals output plus recorded loss", version: "1.0.0",
  dependencies: [{ artifactId: "constitution.Melt.entity", kind: "entity", version: "1.0.0", required: true }],
  law: {
    lawVersion: "0.1", id: "constitution.fine_mass", owner: "constitution", severity: "error",
    rationale: "A fine-metal conversion records its loss instead of silently destroying mass.",
    provenance: { source: "authoring_sdk", sourcePath: "scripts/demo/constitution.source.ts" },
    scope: { root: { kind: "entity", id: "constitution.Melt" }, objectTypes: ["constitution.Melt"] },
    subject: "constitution.Melt", modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.input_quantity == self.output_quantity + self.loss_quantity" },
  },
})

export const Convert = Norn.action({
  id: "constitution.melt.convert", title: "Record a fine-metal conversion", version: "1.0.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Measurements, output: Schema.Struct({ id: Schema.String }),
  derive: [
    { field: "loss_quantity", inputRequired: false, expression: Norn.expression(Measurements, x => x.input_quantity - x.output_quantity) },
  ],
  preconditions: [Norn.expression(Schema.Struct({ input: Measurements }), x => x.input.input_quantity > 0 && x.input.output_quantity >= 0 && x.input.output_quantity <= x.input.input_quantity)],
  writes: [{ type: Melt, change: "created" }],
  effect: { kind: "upsert_input_object", type: Melt, objectIdField: "id", writeMode: { kind: "create_only" } },
})
