import { Schema } from "effect"
import { Norn } from "@norn/authoring"

// CURRENT admission fragment; full REA composition is TARGET in README.md.
export const Melt = Norn.type({
  name: "demo.Melt", title: "Melt measurement", version: "0.1.0",
  properties: Schema.Struct({
    id: Schema.String,
    input_fine_g: Norn.decimal(3, { unit: "g" }),
    output_fine_g: Norn.decimal(3, { unit: "g" }),
    loss_fine_g: Norn.decimal(3, { unit: "g" }),
    status: Schema.Literals(["draft", "measured", "approved", "posted", "rejected"]),
  }),
  identity: [{ name: "primary", properties: ["id"] }],
  defaults: { status: "draft" },
  lifecycles: [{ property: "status", initial: ["draft"], transitions: [
    { id: "measure", from: ["draft"], to: "measured" },
    { id: "approve", from: ["measured"], to: "approved" },
    { id: "reject", from: ["measured"], to: "rejected" },
    { id: "post", from: ["approved"], to: "posted" },
  ] }],
})
