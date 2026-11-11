import { Norn } from "@norn/authoring"

export const RefusalAccess = Norn.access({
  id: "demo.refusal_detail.access",
  title: "Demo operator refusal details",
  version: "1.0.0",
  rules: [
    {
      ruleId: "receive_detail",
      actions: ["norn.disclose_refusal_detail"],
      resourceType: "norn.RefusalDetail",
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["Operator"] } },
        { context: { name: "governedAction", op: "eq", value: "inventory.cycle.receive" } },
      ],
    },
    {
      ruleId: "transfer_detail",
      actions: ["norn.disclose_refusal_detail"],
      resourceType: "norn.RefusalDetail",
      scope: { dimensions: ["org", "environment"] },
      allOf: [
        { role: { anyOf: ["Operator"] } },
        { context: { name: "governedAction", op: "eq", value: "inventory.cycle.transfer" } },
      ],
    },
  ],
})
