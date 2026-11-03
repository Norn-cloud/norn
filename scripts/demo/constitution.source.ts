import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Conversion } from "../../registry/core/src/physics"

const StoredMeasurements = Schema.Struct({
  id: Schema.String,
  input_fine_g: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  output_fine_g: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  loss_fine_g: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
})
const Measurements = Schema.Struct({
  id: Schema.String,
  input_fine_g: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  output_fine_g: Norn.decimal(0, { unit: { system: "ucum", code: "g" } }),
  loss_fine_g: Schema.optionalKey(Norn.decimal(0, { unit: { system: "ucum", code: "g" } })),
})

export const Melt = Norn.type({
  name: "constitution.Melt", title: "Bounded fine-metal conversion", version: "1.0.0",
  role: "happening", implements: [Conversion],
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: StoredMeasurements,
  defaults: { loss_fine_g: 0 },
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
    response: { kind: "predicate", expression: "self.input_fine_g == self.output_fine_g + self.loss_fine_g" },
  },
})

export const Convert = Norn.action({
  id: "constitution.melt.convert", title: "Record a fine-metal conversion", version: "1.0.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  input: Measurements, output: Schema.Struct({ id: Schema.String }),
  derive: [
    { field: "loss_fine_g", inputRequired: false, expression: Norn.expression(Measurements, x => x.input_fine_g - x.output_fine_g) },
  ],
  preconditions: [Norn.expression(Schema.Struct({ input: Measurements }), x => x.input.input_fine_g > 0 && x.input.output_fine_g >= 0 && x.input.output_fine_g <= x.input.input_fine_g)],
  writes: [{ type: Melt, change: "created" }],
  effect: { kind: "upsert_input_object", type: Melt, objectIdField: "id", writeMode: { kind: "create_only" } },
})
