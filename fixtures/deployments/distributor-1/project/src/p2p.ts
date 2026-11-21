// BPIC 2019 (SAP purchase-to-pay, public dataset) through the ordinary mapping
// path: Norn has no event-log (XES/OCEL) import, so each event is a source row.
// GAP canon: no purchase order without quantity and unit (BPIC has neither),
// so the case and its events are types of their own; only vendors reach canon.
import { Norn } from "@norn/authoring"
import { Schema } from "effect"
import { Party } from "../../../../../registry/packs/canon/records.ts"
import { Observed } from "./distributor.ts"

export const P2PCase = Norn.type({
  name: "p2p.Case", title: "Purchase order item (case)", implements: [Observed], version: "0.1.0",
  properties: Schema.Struct({
    case_id: Schema.String, company: Schema.String, document_type: Schema.String, item_type: Schema.String,
    item_category: Schema.String, spend_area: Schema.String, gr_based_invoice: Schema.Boolean, goods_receipt: Schema.Boolean,
  }),
  identity: [{ name: "primary", properties: ["case_id"] }],
  links: [{ id: "vendor", target: Party, cardinality: { to: "one", from: "many" } }],
})

export const P2PEvent = Norn.type({
  name: "p2p.Event", title: "Purchase-to-pay event", implements: [Observed], version: "0.1.0",
  properties: Schema.Struct({
    event_id: Schema.String, activity: Schema.String, occurred_at: Norn.ref("iso8601.timestamp"), user: Schema.String,
    net_worth_eur: Norn.decimal(2, { additivity: { kind: "semi_additive" } }),
  }),
  identity: [{ name: "primary", properties: ["event_id"] }],
  links: [{ id: "case", target: P2PCase, cardinality: { to: "one", from: "many", required: true } }],
})
