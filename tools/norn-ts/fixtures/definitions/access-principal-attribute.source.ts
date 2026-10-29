import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Order = Norn.type({
  name: "commerce.Order",
  title: "Order",
  version: "0.1.0",
  properties: Schema.Struct({ business_unit: Schema.String }),
})

export const OrderAccess = Norn.access({
  id: "commerce.order.access",
  title: "Order access",
  version: "0.1.0",
  rules: [
    {
      ruleId: "reader_in_assigned_units",
      actions: ["norn.read"],
      resourceType: Order,
      scope: { dimensions: ["org"] },
      allOf: [
        {
          resource: {
            name: "business_unit",
            op: "in",
            value: { principalAttribute: "business_units" },
          },
        },
      ],
    },
  ],
})
