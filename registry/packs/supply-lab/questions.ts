import { Norn } from "@norn/authoring"
import { Inspection, InventorySnapshot, OrderLine, ShipmentReceipt, StockMovement } from "./supply.ts"

// Supply-lab questions and their dashboard over the 16 ISO weeks
// 2026-07-06 .. 2026-10-26, beside the pack's definitions (they are compiled
// by the question and dashboard compilers, never installed as artifacts).
// `Norn.metricId` names the metric the Rust checker derives from each type's
// declared additivity; the cell admits each question against that derived
// space (`supply_lab_stress.rs`). `test/supply-lab.test.ts` regenerates
// `questions/supply.questions.json` and `questions/supply.dashboard.json`.

const window = { basis: "effective", from: "2026-07-06T00:00:00Z", to: "2026-10-26T00:00:00Z", freshness: { kind: "live", maxAgeMs: 60000 } } as const

/** METRIC-SNAPSHOT-ASOF-1: each position's last count at or before each week's end, summed across positions, per unit and basis. */
export const OnHandWeekly = Norn.question({
  name: "supply.on_hand_weekly", of: InventorySnapshot,
  measure: Norn.metricId({ kind: "as_of", of: InventorySnapshot, property: "on_hand_qty", time: "as_of" }),
  where: [{ property: "quantity_basis", operator: "eq", value: "on_hand" }],
  by: [{ property: "unit" }, { property: "warehouse_id" }, { property: "as_of", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Additive issued volume per unit and week. */
export const MovementVolumeWeekly = Norn.question({
  name: "supply.movement_volume_weekly", of: StockMovement,
  measure: Norn.metricId({ kind: "sum", of: StockMovement, property: "qty", time: "occurred_at" }),
  where: [{ property: "movement_kind", operator: "eq", value: "issue" }],
  by: [{ property: "unit" }, { property: "occurred_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Supplier fill rate: sum(received) / sum(ordered) over the lines promised in each week, never a mean of line ratios. */
export const FillRateWeekly = Norn.question({
  name: "supply.fill_rate_weekly", of: OrderLine,
  measure: Norn.metricId({ kind: "ratio", of: OrderLine, property: "fill_rate", time: "promised_at" }),
  where: [],
  by: [{ property: "unit" }, { property: "promised_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Received volume per unit by the week each receipt was received. */
export const ReceivedWeekly = Norn.question({
  name: "supply.received_weekly", of: ShipmentReceipt,
  measure: Norn.metricId({ kind: "sum", of: ShipmentReceipt, property: "received_qty", time: "received_at" }),
  where: [],
  by: [{ property: "unit" }, { property: "received_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/**
 * METRIC-GRAIN-LINK-1: inspections have no time of their own, so they are
 * bucketed by their receipt's week through the optional `receipt` link; an
 * inspection with no receipt is counted as unlinked, never bucketed.
 */
export const InspectedByReceiptWeek = Norn.question({
  name: "supply.inspected_by_receipt_week", of: Inspection,
  measure: Norn.metricId({ kind: "sum", of: Inspection, property: "inspected_qty", time: { link: "receipt", property: "received_at" } }),
  where: [{ property: "unit", operator: "eq", value: "each" }],
  by: [{ link: "receipt", property: "received_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

export const SupplyLab = Norn.dashboard({
  title: "Supply lab",
  tiles: [
    { title: "On hand by warehouse, weekly", question: OnHandWeekly, view: "line" },
    { title: "Issued volume, weekly", question: MovementVolumeWeekly, view: "bar" },
    { title: "Supplier fill rate, weekly", question: FillRateWeekly, view: "line" },
    { title: "Received volume, weekly", question: ReceivedWeekly, view: "area" },
    { title: "Inspected by receipt week", question: InspectedByReceiptWeek, view: "bar" },
  ],
})
