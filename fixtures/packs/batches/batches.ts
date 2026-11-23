import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Holding } from "../../../registry/packs/canon/records.ts"

/**
 * Test fixture: a small pack that specializes the canon, installed on a running
 * cell by the HOST-INSTALL-1 tests. A batch turns part of a holding into output;
 * its law says no mass disappears: what went in comes out, or is recorded as
 * loss. Synthetic: no real business or data.
 */
export const Batch = Norn.type({
  name: "lab.Batch", title: "Batch", version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: Schema.Struct({
    id: Schema.String,
    material: Schema.Literals(["copper", "tin"]),
    input_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    output_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_reason: Schema.NullOr(Schema.String),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "holding", target: Holding, cardinality: { to: "one", from: "many" } }],
})

export const MassConserved = Norn.law({
  id: "lab.batch.mass_conserved.law",
  title: "A batch loses no mass: input equals output plus recorded loss.",
  version: "0.1.0",
  law: {
    lawVersion: "0.1",
    id: "lab.batch.mass_conserved",
    owner: "lab",
    severity: "error",
    rationale: "Mass is neither created nor destroyed by a batch; any difference is recorded as loss.",
    provenance: { source: "authoring_sdk", sourcePath: "fixtures/packs/batches/batches.ts" },
    scope: { root: { kind: "entity", id: "lab.Batch" }, objectTypes: ["lab.Batch"] },
    subject: "lab.Batch",
    condition: "true",
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.input_g == self.output_g + self.loss_g" },
  },
  dependencies: [{ artifactId: "lab.Batch.entity", kind: "entity", version: "0.1.0", required: true }],
})

export const RecordBatch = Norn.action({
  id: "lab.batch.record", title: "Record a batch", version: "0.1.0",
  description: "Record how much mass a batch took in, gave out and lost.",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  input: Schema.Struct({
    id: Schema.String, material: Schema.Literals(["copper", "tin"]),
    input_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), output_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), loss_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), loss_reason: Schema.NullOr(Schema.String),
  }),
  output: Schema.Struct({ id: Schema.String }),
  inputLabels: {
    id: { title: "Batch number" },
    material: { title: "Material" },
    input_g: { title: "Mass in (g)" },
    output_g: { title: "Mass out (g)" },
    loss_g: { title: "Recorded loss (g)" },
    loss_reason: { title: "Loss reason", description: "Why mass was lost, when it was." },
  },
  inputGroups: [
    { id: "batch", title: "Batch", fields: ["/id", "/material"] },
    { id: "mass", title: "Mass", fields: ["/input_g", "/output_g", "/loss_g", "/loss_reason"] },
  ],
  writes: [{ type: Batch, change: "created" }, { type: Batch, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Batch, objectIdField: "id" },
  access: { artifactId: "lab.batch.access.policy", version: "0.1.0" },
})

/** Who records a batch on a hosted cell: its members, admins and owner. */
export const BatchAccess = Norn.access({
  id: "lab.batch.access", title: "Batch recorders", version: "0.1.0",
  rules: [
    { ruleId: "recorders", actions: [RecordBatch], resourceType: Batch,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
  ],
})
