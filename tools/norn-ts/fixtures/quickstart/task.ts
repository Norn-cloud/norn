import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Task = Norn.type({
  name: "example.Task",
  title: "Task",
  version: "1.0.0",
  properties: Schema.Struct({ id: Schema.String, title: Schema.String }),
  identity: [{ name: "primary", properties: ["id"] }],
})
