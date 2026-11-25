import { Norn } from "@norn/authoring"

// Supply-lab questions and their dashboard over the 16 ISO weeks
// 2026-07-06 .. 2026-10-26. `measure` names the metric the Rust checker
// derives from each type's declared additivity (`analysis.object_<hex of
// the type name>.<property>.<kind>`); the cell admits each question against
// that derived space (`supply_lab_stress.rs`). `test/supply-lab.test.ts`
// regenerates `supply.questions.json` and `supply.dashboard.json` from here.

const window = { basis: "effective", from: "2026-07-06T00:00:00Z", to: "2026-10-26T00:00:00Z", freshness: { kind: "live", maxAgeMs: 60000 } } as const

/** METRIC-SNAPSHOT-ASOF-1: each position's last count at or before each week's end, summed across positions, per unit and basis. */
export const OnHandWeekly = Norn.question({
  name: "supply.on_hand_weekly", of: "supply.InventorySnapshot",
  measure: "analysis.object_737570706c792e496e76656e746f7279536e617073686f74.on_hand_qty.as_of_as_of",
  where: [{ property: "quantity_basis", operator: "eq", value: "on_hand" }],
  by: [{ property: "unit" }, { property: "warehouse_id" }, { property: "as_of", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Additive issued volume per unit and week. */
export const MovementVolumeWeekly = Norn.question({
  name: "supply.movement_volume_weekly", of: "supply.StockMovement",
  measure: "analysis.object_737570706c792e53746f636b4d6f76656d656e74.qty.by_occurred_at",
  where: [{ property: "movement_kind", operator: "eq", value: "issue" }],
  by: [{ property: "unit" }, { property: "occurred_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Supplier fill rate: sum(received) / sum(ordered) over the lines promised in each week, never a mean of line ratios. */
export const FillRateWeekly = Norn.question({
  name: "supply.fill_rate_weekly", of: "supply.OrderLine",
  measure: "analysis.object_737570706c792e4f726465724c696e65.fill_rate.by_promised_at.ratio_of_sums",
  where: [],
  by: [{ property: "unit" }, { property: "promised_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

/** Received volume per unit by the week each receipt was received. */
export const ReceivedWeekly = Norn.question({
  name: "supply.received_weekly", of: "supply.ShipmentReceipt",
  measure: "analysis.object_737570706c792e536869706d656e7452656365697074.received_qty.by_received_at",
  where: [],
  by: [{ property: "unit" }, { property: "received_at", grain: "week" }],
  asOf: window, population: { kind: "authorized_objects" },
})

export const SupplyLab = Norn.dashboard({
  title: "Supply lab",
  tiles: [
    { title: "On hand by warehouse, weekly", question: OnHandWeekly, view: "line" },
    { title: "Issued volume, weekly", question: MovementVolumeWeekly, view: "bar" },
    { title: "Supplier fill rate, weekly", question: FillRateWeekly, view: "line" },
    { title: "Received volume, weekly", question: ReceivedWeekly, view: "area" },
  ],
})
