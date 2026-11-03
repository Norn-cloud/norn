import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// CURRENT admission fragment; full REA composition is TARGET in README.md.
export const Melt = Norn.type({
  name: "demo.Melt", title: "Melt measurement", version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity", required: true }],
  properties: Schema.Struct({
    id: Schema.String,
    input_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    output_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    status: Schema.Literals(["draft", "measured", "approved", "posted", "rejected"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], terminal: ["posted", "rejected"], immutable: ["posted"], transitions: [
    { id: "measure", from: ["draft"], to: "measured" },
    { id: "approve", from: ["measured"], to: "approved" },
    { id: "reject", from: ["measured"], to: "rejected" },
    { id: "post", from: ["approved"], to: "posted" },
  ] }],
})

export const FineMassConservation = Norn.law({
  id: "demo.fine_mass_conservation.law",
  title: "Input fine mass equals output plus recorded loss.",
  version: "0.1.0",
  law: {
    lawVersion: "0.1",
    id: "demo.fine_mass_conservation",
    owner: "demo.jewelry",
    severity: "error",
    rationale: "Input fine mass equals output plus explicitly recorded loss.",
    provenance: { source: "authoring_sdk", sourcePath: "scripts/demo/jewelry.source.ts" },
    scope: { root: { kind: "app", id: "demo.jewelry" }, objectTypes: ["demo.Melt"] },
    subject: "demo.Melt",
    condition: "self.status == 'posted'",
    modality: { kind: "always" },
    response: {
      kind: "predicate",
      expression: "self.input_fine_g == self.output_fine_g + self.loss_fine_g",
    },
  },
  dependencies: [{ artifactId: "demo.Melt.entity", kind: "entity", version: "0.1.0", required: true }],
})
