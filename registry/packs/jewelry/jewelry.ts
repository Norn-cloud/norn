import { Schema } from "effect"
import { Norn } from "@norn/authoring"
import { Holding } from "../canon/records.ts"

/**
 * Jewelry: a specialization of the canon, not a rebuild of it. A melt turns a
 * holding's gold or silver into fine metal; its law says no fine metal
 * disappears: what went in comes out, or is recorded as loss. The canon's
 * types, laws and data stay as they are; this pack only adds to them.
 * Synthetic demo pack: no real business or data.
 */
export const Melt = Norn.type({
  name: "jewelry.Melt", title: "Melt", version: "0.1.0",
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: Schema.Struct({
    id: Schema.String,
    metal: Schema.Literals(["gold", "silver"]),
    input_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    output_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_reason: Schema.NullOr(Schema.String),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  links: [{ id: "holding", target: Holding, cardinality: { to: "one", from: "many" } }],
})

export const FineMassConserved = Norn.law({
  id: "jewelry.melt.fine_mass_conserved.law",
  title: "A melt loses no fine metal: input equals output plus recorded loss.",
  version: "0.1.0",
  law: {
    lawVersion: "0.1",
    id: "jewelry.melt.fine_mass_conserved",
    owner: "jewelry",
    severity: "error",
    rationale: "Fine metal is neither created nor destroyed by a melt; any difference is recorded as loss.",
    provenance: { source: "authoring_sdk", sourcePath: "registry/packs/jewelry/jewelry.ts" },
    scope: { root: { kind: "entity", id: "jewelry.Melt" }, objectTypes: ["jewelry.Melt"] },
    subject: "jewelry.Melt",
    condition: "true",
    modality: { kind: "always" },
    response: { kind: "predicate", expression: "self.input_fine_g == self.output_fine_g + self.loss_fine_g" },
  },
  dependencies: [{ artifactId: "jewelry.Melt.entity", kind: "entity", version: "0.1.0", required: true }],
})

export const RecordMelt = Norn.action({
  id: "jewelry.melt.record", title: "Record a melt", version: "0.1.0",
  description: "Record how much fine metal a melt took in, gave out and lost.",
  scopeRequired: ["orgId", "environmentId"], idempotent: true, ledgered: true,
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  input: Schema.Struct({
    id: Schema.String, metal: Schema.Literals(["gold", "silver"]),
    input_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), output_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), loss_fine_g: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }), loss_reason: Schema.NullOr(Schema.String),
  }),
  output: Schema.Struct({ id: Schema.String }),
  inputLabels: {
    id: { title: "Melt number" },
    metal: { title: "Metal" },
    input_fine_g: { title: "Fine metal in (g)" },
    output_fine_g: { title: "Fine metal out (g)" },
    loss_fine_g: { title: "Recorded loss (g)" },
    loss_reason: { title: "Loss reason", description: "Why fine metal was lost, when it was." },
  },
  inputGroups: [
    { id: "melt", title: "Melt", fields: ["/id", "/metal"] },
    { id: "fine", title: "Fine metal", fields: ["/input_fine_g", "/output_fine_g", "/loss_fine_g", "/loss_reason"] },
  ],
  writes: [{ type: Melt, change: "created" }, { type: Melt, change: "modified" }],
  effect: { kind: "upsert_input_object", type: Melt, objectIdField: "id" },
  access: { artifactId: "jewelry.melt.access.policy", version: "0.1.0" },
})

/** Who records a melt on a hosted cell: its members, admins and owner. */
export const MeltAccess = Norn.access({
  id: "jewelry.melt.access", title: "Melt recorders", version: "0.1.0",
  rules: [
    { ruleId: "recorders", actions: [RecordMelt], resourceType: Melt,
      scope: { dimensions: ["org", "environment"] }, allOf: [{ role: { anyOf: ["member", "admin", "owner"] } }] },
  ],
})
