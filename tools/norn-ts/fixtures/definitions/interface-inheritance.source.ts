import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Named = Norn.interfaceType({
  name: "inheritance.Named", title: "Named", version: "0.1.0",
  properties: Schema.Struct({ code: Schema.String }),
})

export const Located = Norn.interfaceType({
  name: "inheritance.Located", title: "Located", version: "0.1.0",
  extends: [Named], properties: Schema.Struct({}),
  invariants: [{ id: "code_present", expression: Norn.unsafeZen<boolean>('self.code != ""'), message: "code is present" }],
})
