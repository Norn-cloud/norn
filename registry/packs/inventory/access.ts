/**
 * Fail-closed action-level AccessIR policy for all Inventory & Warehouse domain actions.
 *
 * Compiles to `inventory.access.policy` (referenced by every action in `registry/packs/inventory/`).
 * Distinct roles: `warehouse_clerk`, `inventory_manager`, `quality_inspector`, `owner`.
 */
import { Norn } from "@norn/authoring"
import {
  ApproveCycleCount,
  ConsumeReservation,
  CreateReservation,
  CycleCount,
  PostCycleCount,
  RecordMovement,
  ReleaseReservation,
  StockMovement,
  StockReservation,
  SubmitCycleCount,
} from "./stock.ts"
import { LotBatch, QuarantineLot, RecallLot } from "./warehouse.ts"

export const InventoryAccess = Norn.access({
  id: "inventory.access",
  title: "Inventory and warehouse action authorization policy",
  version: "0.1.0",
  rules: [
    {
      ruleId: "record_stock_movement",
      actions: [RecordMovement],
      resourceType: StockMovement,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["warehouse_clerk", "inventory_manager", "owner"] } }],
    },
    {
      ruleId: "manage_stock_reservation",
      actions: [CreateReservation, ConsumeReservation, ReleaseReservation],
      resourceType: StockReservation,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["warehouse_clerk", "inventory_manager", "owner"] } }],
    },
    {
      ruleId: "quarantine_lot",
      actions: [QuarantineLot],
      resourceType: LotBatch,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["warehouse_clerk", "quality_inspector", "inventory_manager", "owner"] } }],
    },
    {
      ruleId: "recall_lot",
      actions: [RecallLot],
      resourceType: LotBatch,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["quality_inspector", "inventory_manager", "owner"] } }],
    },
    {
      ruleId: "submit_cycle_count",
      actions: [SubmitCycleCount],
      resourceType: CycleCount,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["warehouse_clerk", "inventory_manager", "owner"] } }],
    },
    {
      ruleId: "approve_or_post_cycle_count",
      actions: [ApproveCycleCount, PostCycleCount],
      resourceType: CycleCount,
      scope: { dimensions: ["org", "environment"] },
      allOf: [{ role: { anyOf: ["inventory_manager", "owner"] } }],
    },
  ],
})
