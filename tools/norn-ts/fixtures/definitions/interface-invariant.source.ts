import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const TotalBearing = Norn.interfaceType({
  name: "commerce.TotalBearing",
  title: "Total bearing",
  version: "0.1.0",
  properties: Schema.Struct({ total: Schema.Int }),
  invariants: [
    {
      id: "non_negative_total",
      classification: "quality",
      expression: Norn.unsafeZen<boolean>("self.total >= 0"),
      message: "total must be non-negative",
    },
  ],
})

export const LegacyInterface = Norn.interfaceType({
  name: "commerce.LegacyInterface",
  title: "Legacy interface",
  version: "0.1.0",
  properties: Schema.Struct({ name: Schema.String }),
  invariants: [
    {
      id: "name_present_default_integrity",
      expression: Norn.unsafeZen<boolean>('self.name != ""'),
      message: "name must not be empty",
    },
    {
      id: "name_present_explicit_integrity",
      classification: "integrity",
      expression: Norn.unsafeZen<boolean>('self.name != ""'),
      message: "name must not be empty",
    },
  ],
})

export const EmptyInterface = Norn.interfaceType({
  name: "commerce.EmptyInterface",
  title: "Empty interface",
  version: "0.1.0",
  properties: Schema.Struct({ name: Schema.String }),
})
