import { Schema } from "effect"
import { Norn } from "@norn/authoring"

const Masses = Schema.Struct({
  input_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
  output_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
  loss_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
})

export const ConservedFineMass = Norn.interfaceType({
  name: "demo.ConservedFineMass", title: "Conserved fine mass", version: "0.1.0",
  dependencies: [{ artifactId: "core.units", kind: "entity", version: "1.0.0", required: true }],
  properties: Masses,
  invariants: [{
    id: "fine_mass_conservation", classification: "integrity",
    expression: Norn.expression(Schema.Struct({ input_fine_g: Schema.Finite, output_fine_g: Schema.Finite, loss_fine_g: Schema.Finite }), input =>
      input.input_fine_g >= 0 && input.output_fine_g >= 0 && input.loss_fine_g >= 0 &&
      input.input_fine_g === input.output_fine_g + input.loss_fine_g),
    message: "Input fine mass must equal output plus recorded loss; all masses must be non-negative.",
  }],
})

export const MeltMeasurement = Norn.type({
  name: "demo.MeltMeasurement", title: "Melt measurement", version: "0.1.0",
  dependencies: [{ artifactId: "core.units", kind: "entity", version: "1.0.0", required: true }],
  implements: [ConservedFineMass], immutable: true,
  properties: Schema.Struct({
    id: Schema.String,
    input_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    output_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
})

export const FineMassConservation = Norn.law({
  id: "demo.fine_mass_conservation.law", title: "Record every fine gram", version: "0.1.0",
  law: {
    lawVersion: "0.1", id: "demo.fine_mass_conservation", owner: "demo.MeltMeasurement",
    severity: "error",
    rationale: "For each recorded melt measurement, input fine mass shall equal output plus recorded loss in the same commit.",
    provenance: { source: "authoring_sdk", sourcePath: "tools/norn-ts/fixtures/melt-surfaces/melt.source.ts" },
    scope: { root: { kind: "entity", id: "demo.MeltMeasurement" }, objectTypes: ["demo.MeltMeasurement"] },
    subject: "demo.MeltMeasurement", modality: { kind: "always" },
    response: { kind: "invariant", objectType: "demo.MeltMeasurement", invariantId: "fine_mass_conservation" },
  },
  dependencies: [{ artifactId: "demo.MeltMeasurement.entity", kind: "entity", version: "0.1.0", required: true }],
})

export const RecordMelt = Norn.action({
  id: "demo.melt.record", title: "Record melt measurement", version: "0.1.0",
  input: Schema.Struct({
    id: Schema.String,
    input_fine_g: Norn.decimal(3), output_fine_g: Norn.decimal(3), loss_fine_g: Norn.decimal(3),
  }),
  output: Schema.Struct({ id: Schema.String }),
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  writes: [{ type: MeltMeasurement, change: "created" }],
  effect: { kind: "upsert_input_object", type: MeltMeasurement, objectIdField: "id", writeMode: { kind: "create_only" } },
})
