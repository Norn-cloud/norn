/**
 * Actions the stand-in cell serves on `GET /v1/actions`, in DEFS's input dialect
 * (`wip/defs-dialect`): `title` and `description` on nodes, `enum` on a string input that writes
 * an enum property, and `x-norn.groups` on the object node. Shared by the unit tests and the
 * Playwright stand-in so both read the same document.
 */

/** The kernel's effective effects; the cell always sends them. */
const NONE = { irreversible: false, destructive: false, money: [] as string[] }

const decimal = (unit: string, scale: number) => ({ type: "number", nornType: { kind: "decimal", scale, unit: { code: unit, system: "ucum" } } })

/** Upsert: open a holding. Uses all three dialect additions. */
export const OPEN_HOLDING = {
  actionId: "canon.holding.open",
  title: "Open a holding",
  description: "Record stock held at a site, with how its cost is worked out.",
  objectKind: "canon.Holding",
  effect: "upsert_input_object",
  objectIdField: "id",
  uiEffects: NONE,
  inputSchema: {
    type: "object",
    additionalProperties: false,
    required: ["id", "resource_code", "quantity", "valuation_method", "location"],
    properties: {
      id: { type: "string", title: "Holding id" },
      resource_code: { type: "string", title: "Resource code", description: "The item's code in the source system, like R-0001." },
      quantity: { ...decimal("g", 3), title: "Quantity" },
      valuation_method: { type: "string", title: "Valuation method", description: "How the cost of stock is worked out when it leaves.", enum: ["fifo", "moving_average"] },
      location: {
        type: "object", title: "Location", additionalProperties: false, required: ["site"],
        properties: { site: { type: "string", title: "Site" }, bin: { type: ["string", "null"] } },
      },
      note: { type: ["string", "null"] },
    },
    "x-norn": {
      groups: [
        { id: "what", title: "What is held", fields: ["/resource_code", "/quantity", "/valuation_method"] },
        { id: "where", title: "Where it is", fields: ["/location/site"] },
      ],
    },
  },
} as const

/** Transition: adjust a holding's quantity, with a reason. */
export const ADJUST_HOLDING = {
  actionId: "canon.holding.adjust",
  title: "Adjust quantity",
  description: "Correct the quantity after a count.",
  objectKind: "canon.Holding",
  effect: "fire_transition",
  objectIdField: "id",
  uiEffects: NONE,
  inputSchema: {
    type: "object", additionalProperties: false, required: ["id", "quantity", "reason"],
    properties: { id: { type: "string", title: "Holding id" }, quantity: { ...decimal("g", 3), title: "Counted quantity" }, reason: { type: "string", title: "Reason" } },
    // One group: a single-page form with a titled section (two or more groups make a wizard).
    "x-norn": { groups: [{ id: "count", title: "The count", fields: ["/quantity", "/reason"] }] },
  },
} as const

/** Transition the declaration marks destructive and irreversible: Studio confirms before it calls. */
export const RETIRE_HOLDING = {
  actionId: "canon.holding.retire",
  title: "Retire holding",
  description: "Close a holding for good; it stops counting towards stock.",
  objectKind: "canon.Holding",
  effect: "fire_transition",
  objectIdField: "id",
  uiEffects: { irreversible: true, destructive: true, money: [] },
  inputSchema: { type: "object", additionalProperties: false, required: ["id"], properties: { id: { type: "string", title: "Holding id" } } },
} as const

/** Parent plus owned rows (`mutate_parent_with_rows`): offered like an upsert on the parent's type. */
export const RESTOCK_HOLDING = {
  actionId: "canon.holding.restock",
  title: "Restock holding",
  description: "Add lots to a holding; each lot is a row the holding owns.",
  objectKind: "canon.Holding",
  effect: "mutate_parent_with_rows",
  objectIdField: "id",
  uiEffects: NONE,
  inputSchema: {
    type: "object", additionalProperties: false, required: ["id", "lots"],
    properties: {
      id: { type: "string", title: "Holding id" },
      lots: {
        type: "array", title: "Lots",
        items: { type: "object", additionalProperties: false, required: ["lot", "quantity"], properties: { lot: { type: "string", title: "Lot" }, quantity: { ...decimal("g", 3), title: "Quantity" } } },
      },
    },
  },
} as const

export const ACTIONS = [OPEN_HOLDING, RESTOCK_HOLDING, ADJUST_HOLDING, RETIRE_HOLDING] as const
