# Governed Inventory & Warehouse Domain Pack

Jurisdiction-agnostic inventory and warehouse ontology built on Norn Core REA (`core.Agent`, `core.Resource`, `core.Hold`, `core.EconomicEvent`) and `core.TimeInterval`. All IR artifacts under `artifacts/` are generated from the typed TypeScript sources (`warehouse.ts`, `stock.ts`, `access.ts`).

## Model

- **Facilities & catalog (`warehouse.ts`):** `inventory.Warehouse` (`core.Agent`, acyclic `parent`), `inventory.BinLocation`, `inventory.Item` (catalog metadata, never stock), and `inventory.LotBatch` (`quarantine`, `release_quarantine`, `expire`, `recall` with terminal immutable `recalled`).
- **Stock, holds, movements & counts (`stock.ts`):** `inventory.StockBalance` (`core.Resource`), `inventory.StockReservation` (`core.Hold`, 3-day `expire`), `inventory.StockMovement` (`core.EconomicEvent`, single-state terminal immutable `recorded` lifecycle), and `inventory.CycleCount` (`core.TimeInterval`, `submit -> approve -> post`, immutable on `approved` and `posted`).
- **Access (`access.ts`):** fail-closed Cedar `inventory.access.policy` scoped by `org` and `environment` across `warehouse_clerk`, `quality_inspector`, `inventory_manager`, and `owner`.

## Scope and Honest Boundaries

- **Action & decision-preparation immutability:** `inventory.movement.record` and `inventory.reservation.create` enforce `writeMode: { kind: "create_only" }` at the action boundary (preventing duplicate creation through those actions), and immutable lifecycle states (`StockMovement.recorded`, `LotBatch.recalled`, `CycleCount.approved`, `CycleCount.posted`) refuse property-set mutations (`DecisionRefusal::ImmutableStateWrite`) during kernel decision preparation (`decision_commit.rs`). Raw low-level `CellHost::commit` writes and object deletions remain outside lifecycle-state guards and depend on host/storage writer discipline.
- **Single-object action writes:** The current `ActionEffect` profile supports one target object per `upsert_input_object` / `fire_transition` action. `inventory.movement.record` and `inventory.reservation.create` validate active warehouses (provider, receiver/holder, and the balance's own `owner`/`custodian` warehouses), active linked bin/lot, `(action, movement_kind)` consistency, unit match, and unreserved stock availability (`quantity - reserved_quantity`), and atomically commit the event or reservation plus its links; updating `StockBalance.quantity` / `StockBalance.reserved_quantity` is not yet folded into the same action commit.
- **Formal verification enrollment (`inventory-authority`):** Runtime execution, checker admission, and `LAW-EXAMPLE-1` cases are covered here. Full `inventory-authority` subsystem enrollment (Quint models, Lean 4 proofs, and promoted formal gates in `verification/coverage-v1/`) is a separate held-domain milestone tracked by `scripts/proof_enrollment.py`.
