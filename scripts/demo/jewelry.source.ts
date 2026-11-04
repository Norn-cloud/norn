import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// CURRENT admission fragment; full REA composition is TARGET in README.md.
export const Melt = Norn.type({
  dependencies: [{ artifactId: "core.units", version: "1.0.0" }],
  name: "demo.Melt", title: "Melt measurement", version: "0.1.0", role: "happening",
  implements: ["core.Conversion"],
  properties: Schema.Struct({
    id: Schema.String,
    input_quantity: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    output_quantity: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
    loss_quantity: Norn.decimal(3, { unit: { system: "ucum", code: "g" } }),
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
