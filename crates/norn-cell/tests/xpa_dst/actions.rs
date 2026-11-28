//! Domain action helpers for [`super::env::EntityCell`] over
//! `registry/packs/supply-lab/` omitting kernel-derived fields (`ORIGIN-DATAFLOW-1`).

use serde_json::json;

use super::env::{
    COSTS_FEED_ID, COUNTS_FEED_ID, EntityCell, FeedRow, POS_FEED_ID, RECEIPTS_FEED_ID,
};

/// Input parameters for recording a verified `supply.StoreCommercialRollup`.
pub struct StoreKpiInput<'a> {
    pub rollup_id: &'a str,
    pub store_id: &'a str,
    pub sku_id: &'a str,
    pub period_week: &'a str,
    pub unit: &'a str,
    pub on_hand_count_id: &'a str,
    pub opening_cut_id: &'a str,
    pub count_ref: &'a str,
    pub count_as_of: &'a str,
    pub costed_ref: &'a str,
    pub period_start: &'a str,
    pub period_end: &'a str,
    pub sales_weeks: i64,
    pub on_hand_qty: &'a str,
    pub avg_weekly_sales_qty: &'a str,
    pub period_gross_margin: &'a str,
    pub opening_stock_value: &'a str,
    pub closing_stock_value: &'a str,
}

impl EntityCell {
    /// Register this cell's single `LegalEntity` book.
    ///
    /// # Errors
    /// Returns an error if registration fails.
    pub fn register_book(&mut self, legal_name: &str, entity_kind: &str) -> Result<(), String> {
        let entity_id = self.entity_id.clone();
        let currency = self.currency.clone();
        self.ok(
            "supply.legal_entity.register",
            "eng:dina",
            json!({
                "entity_id": entity_id,
                "legal_name": legal_name,
                "entity_kind": entity_kind,
                "book_code": format!("BOOK-{entity_id}-{currency}"),
                "functional_currency": currency,
            }),
        )
    }

    /// Register a SKU in this cell.
    ///
    /// # Errors
    /// Returns an error if registration fails.
    pub fn register_sku(&mut self, sku_id: &str, category: &str, uom: &str) -> Result<(), String> {
        self.ok(
            "supply.sku.register",
            "eng:dina",
            json!({
                "sku_id": sku_id,
                "category": category,
                "unit_of_measure": uom,
                "shelf_life_days": 365,
            }),
        )
    }

    /// Register a store owned by this cell's `LegalEntity`.
    ///
    /// # Errors
    /// Returns an error if registration fails.
    pub fn register_store(
        &mut self,
        store_id: &str,
        name: &str,
        custodian_id: &str,
        location_kind: &str,
        mall_code: Option<&str>,
    ) -> Result<(), String> {
        let entity_id = self.entity_id.clone();
        let currency = self.currency.clone();
        self.ok(
            "supply.store.register",
            "eng:dina",
            json!({
                "store_id": store_id,
                "name": name,
                "entity_id": entity_id,
                "custodian_id": custodian_id,
                "location_kind": location_kind,
                "mall_code": mall_code,
                "currency": currency,
            }),
        )
    }

    /// Record a governed SKU `UnitCost` (create-only per SKU in `supply-lab`).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    pub fn record_unit_cost(
        &mut self,
        sku_id: &str,
        cost_ref: &str,
        supplier_id: &str,
        unit_cost: &str,
        effective_from: &str,
    ) -> Result<(), String> {
        let entity_id = self.entity_id.clone();
        let currency = self.currency.clone();
        self.ok(
            "supply.unit_cost.record",
            "eng:dina",
            json!({
                "sku_id": sku_id,
                "cost_ref": cost_ref,
                "owner_entity_id": entity_id,
                "supplier_id": supplier_id,
                "unit_cost": unit_cost,
                "currency": currency,
                "effective_from": effective_from,
            }),
        )
    }

    /// Record a costed `StockLot` omitting kernel-derived fields (`ORIGIN-DATAFLOW-1`).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for governed stock lot"
    )]
    pub fn record_costed_lot(
        &mut self,
        lot_id: &str,
        store_id: &str,
        sku_id: &str,
        custodian_id: &str,
        received_qty: &str,
        unit: &str,
        unit_cost: &str,
        received_at: &str,
    ) -> Result<(), String> {
        let payload = self.costed_lot_payload(
            lot_id,
            store_id,
            sku_id,
            custodian_id,
            received_qty,
            unit,
            unit_cost,
            received_at,
        );
        self.ok("supply.stock_lot.record", "clerk:hana", payload)
    }

    /// Record a costed `StockLot` with an explicit idempotency key (used for
    /// bilateral inter-entity transfer correlation).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields plus transfer idempotency key"
    )]
    pub fn record_costed_lot_keyed(
        &mut self,
        lot_id: &str,
        store_id: &str,
        sku_id: &str,
        custodian_id: &str,
        received_qty: &str,
        unit: &str,
        unit_cost: &str,
        received_at: &str,
        key: &str,
    ) -> Result<(), String> {
        let payload = self.costed_lot_payload(
            lot_id,
            store_id,
            sku_id,
            custodian_id,
            received_qty,
            unit,
            unit_cost,
            received_at,
        );
        self.ok_keyed("supply.stock_lot.record", "clerk:hana", payload, key)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "shared JSON payload builder for governed costed stock lot"
    )]
    fn costed_lot_payload(
        &self,
        lot_id: &str,
        store_id: &str,
        sku_id: &str,
        custodian_id: &str,
        received_qty: &str,
        unit: &str,
        unit_cost: &str,
        received_at: &str,
    ) -> serde_json::Value {
        json!({
            "lot_id": lot_id,
            "store_id": store_id,
            "sku_id": sku_id,
            "owner_entity_id": self.entity_id,
            "custodian_id": custodian_id,
            "received_qty": received_qty,
            "remaining_qty": received_qty,
            "unit": unit,
            "received_at": received_at,
            "as_of": received_at,
            "cost_status": "costed",
            "unit_cost": unit_cost,
            "currency": self.currency,
        })
    }

    /// Record an uncosted `StockLot` omitting kernel-derived fields (`ORIGIN-DATAFLOW-1`).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for uncosted stock lot"
    )]
    pub fn record_uncosted_lot(
        &mut self,
        lot_id: &str,
        store_id: &str,
        sku_id: &str,
        custodian_id: &str,
        received_qty: &str,
        unit: &str,
        received_at: &str,
    ) -> Result<(), String> {
        let entity_id = self.entity_id.clone();
        let currency = self.currency.clone();
        self.ok(
            "supply.stock_lot.record_uncosted",
            "clerk:hana",
            json!({
                "lot_id": lot_id,
                "store_id": store_id,
                "sku_id": sku_id,
                "owner_entity_id": entity_id,
                "custodian_id": custodian_id,
                "received_qty": received_qty,
                "remaining_qty": received_qty,
                "unit": unit,
                "received_at": received_at,
                "as_of": received_at,
                "cost_status": "missing_cost",
                "currency": currency,
            }),
        )
    }

    /// Update a costed `StockLot` balance omitting kernel-derived fields (`ORIGIN-DATAFLOW-1`).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for stock lot balance update"
    )]
    pub fn update_lot_balance(
        &mut self,
        lot_id: &str,
        store_id: &str,
        sku_id: &str,
        remaining_qty: &str,
        depleted_qty: &str,
        unit_cost: &str,
        as_of: &str,
    ) -> Result<(), String> {
        self.ok(
            "supply.stock_lot.update_balance",
            "clerk:hana",
            json!({
                "lot_id": lot_id,
                "store_id": store_id,
                "sku_id": sku_id,
                "remaining_qty": remaining_qty,
                "depleted_qty": depleted_qty,
                "as_of": as_of,
                "unit_cost": unit_cost,
            }),
        )
    }

    /// Record a costed `PosSale` omitting kernel-derived fields (`ORIGIN-DATAFLOW-1`).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for costed POS sale"
    )]
    pub fn record_costed_sale(
        &mut self,
        sale_id: &str,
        store_id: &str,
        sku_id: &str,
        sold_qty: &str,
        unit: &str,
        net_revenue: &str,
        applied_unit_cost: &str,
        costed_ref: &str,
        sold_at: &str,
    ) -> Result<(), String> {
        let payload = self.costed_sale_payload(
            sale_id,
            store_id,
            sku_id,
            sold_qty,
            unit,
            net_revenue,
            applied_unit_cost,
            costed_ref,
            sold_at,
        );
        self.ok("supply.pos_sale.record", "clerk:hana", payload)
    }

    /// Record a costed `PosSale` with an explicit idempotency key (used for
    /// bilateral inter-entity transfer correlation).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields plus transfer idempotency key"
    )]
    pub fn record_costed_sale_keyed(
        &mut self,
        sale_id: &str,
        store_id: &str,
        sku_id: &str,
        sold_qty: &str,
        unit: &str,
        net_revenue: &str,
        applied_unit_cost: &str,
        costed_ref: &str,
        sold_at: &str,
        key: &str,
    ) -> Result<(), String> {
        let payload = self.costed_sale_payload(
            sale_id,
            store_id,
            sku_id,
            sold_qty,
            unit,
            net_revenue,
            applied_unit_cost,
            costed_ref,
            sold_at,
        );
        self.ok_keyed("supply.pos_sale.record", "clerk:hana", payload, key)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "shared JSON payload builder for governed costed POS sale"
    )]
    fn costed_sale_payload(
        &self,
        sale_id: &str,
        store_id: &str,
        sku_id: &str,
        sold_qty: &str,
        unit: &str,
        net_revenue: &str,
        applied_unit_cost: &str,
        costed_ref: &str,
        sold_at: &str,
    ) -> serde_json::Value {
        json!({
            "sale_id": sale_id,
            "store_id": store_id,
            "sku_id": sku_id,
            "sold_qty": sold_qty,
            "unit": unit,
            "net_revenue": net_revenue,
            "applied_unit_cost": applied_unit_cost,
            "cost_status": "costed",
            "costed_ref": costed_ref,
            "currency": self.currency,
            "sold_at": sold_at,
        })
    }

    /// Record an uncosted `PosSale` omitting kernel-derived fields (`ORIGIN-DATAFLOW-1`).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for uncosted POS sale"
    )]
    pub fn record_uncosted_sale(
        &mut self,
        sale_id: &str,
        store_id: &str,
        sku_id: &str,
        sold_qty: &str,
        unit: &str,
        net_revenue: &str,
        sold_at: &str,
    ) -> Result<(), String> {
        let currency = self.currency.clone();
        self.ok(
            "supply.pos_sale.record_uncosted",
            "clerk:hana",
            json!({
                "sale_id": sale_id,
                "store_id": store_id,
                "sku_id": sku_id,
                "sold_qty": sold_qty,
                "unit": unit,
                "net_revenue": net_revenue,
                "cost_status": "missing_cost",
                "currency": currency,
                "sold_at": sold_at,
            }),
        )
    }

    /// Record an initial physical `StoreCount` or recount an existing count position.
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for store count"
    )]
    pub fn record_store_count(
        &mut self,
        recount: bool,
        store_count_id: &str,
        store_id: &str,
        sku_id: &str,
        on_hand_qty: &str,
        unit: &str,
        quantity_basis: &str,
        as_of: &str,
        count_ref: &str,
    ) -> Result<(), String> {
        let action = if recount {
            "supply.store_count.recount"
        } else {
            "supply.store_count.record"
        };
        self.ok(
            action,
            "clerk:hana",
            json!({
                "store_count_id": store_count_id,
                "store_id": store_id,
                "sku_id": sku_id,
                "on_hand_qty": on_hand_qty,
                "unit": unit,
                "quantity_basis": quantity_basis,
                "as_of": as_of,
                "count_source": "pos_cycle_count",
                "count_ref": count_ref,
            }),
        )
    }

    /// Record an immutable `supply.StockValuationCut` for `(store_id, sku_id)`.
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for immutable stock valuation cut"
    )]
    pub fn record_stock_valuation_cut(
        &mut self,
        valuation_cut_id: &str,
        store_id: &str,
        sku_id: &str,
        unit: &str,
        valued_at: &str,
        remaining_qty: &str,
        stock_value: &str,
    ) -> Result<(), String> {
        let currency = self.currency.clone();
        self.ok(
            "supply.stock_valuation_cut.record",
            "planner:omar",
            json!({
                "cut_id": valuation_cut_id,
                "store_id": store_id,
                "sku_id": sku_id,
                "unit": unit,
                "currency": currency,
                "cost_status": "costed",
                "book_epoch": "1970-01-01T00:00:00Z",
                "cut_at": valued_at,
                "remaining_qty": remaining_qty,
                "stock_value": stock_value,
            }),
        )
    }

    /// Record a verified `supply.StoreCommercialRollup` omitting kernel-derived fields.
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    pub fn record_store_kpi(&mut self, kpi: &StoreKpiInput<'_>) -> Result<(), String> {
        let payload = self.store_kpi_payload(kpi);
        self.ok("supply.store_kpi.record", "planner:omar", payload)
    }

    /// Refresh an existing `supply.StoreCommercialRollup` in-place (`update_only`).
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    pub fn refresh_store_kpi(&mut self, kpi: &StoreKpiInput<'_>) -> Result<(), String> {
        let payload = self.store_kpi_payload(kpi);
        self.ok("supply.store_kpi.refresh", "planner:omar", payload)
    }

    fn store_kpi_payload(&self, kpi: &StoreKpiInput<'_>) -> serde_json::Value {
        json!({
            "rollup_id": kpi.rollup_id,
            "store_id": kpi.store_id,
            "sku_id": kpi.sku_id,
            "period_week": kpi.period_week,
            "unit": kpi.unit,
            "currency": self.currency,
            "cost_status": "costed",
            "count_status": "verified",
            "on_hand_count_id": kpi.on_hand_count_id,
            "opening_cut_id": kpi.opening_cut_id,
            "count_ref": kpi.count_ref,
            "count_as_of": kpi.count_as_of,
            "costed_ref": kpi.costed_ref,
            "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": kpi.period_start,
            "period_end": kpi.period_end,
            "sales_weeks": kpi.sales_weeks,
            "on_hand_qty": kpi.on_hand_qty,
            "avg_weekly_sales_qty": kpi.avg_weekly_sales_qty,
            "period_gross_margin": kpi.period_gross_margin,
            "opening_stock_value": kpi.opening_stock_value,
            "closing_stock_value": kpi.closing_stock_value,
        })
    }

    /// Register a `supply.Warehouse` in this cell.
    ///
    /// # Errors
    /// Returns an error if registration fails.
    pub fn register_warehouse(&mut self, warehouse_id: &str, name: &str) -> Result<(), String> {
        self.ok(
            "supply.warehouse.register",
            "eng:dina",
            json!({
                "warehouse_id": warehouse_id,
                "name": name,
                "region": "delta",
                "latitude": "30.044400",
                "longitude": "31.235700",
            }),
        )
    }

    /// Open (`create_only`) or audit-restate (`update_only`) an existing
    /// `supply.InventorySnapshot` position.
    ///
    /// # Errors
    /// Returns an error if the action refuses.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit domain fields for inventory position open/restate"
    )]
    pub fn record_inventory_position(
        &mut self,
        restate: bool,
        snapshot_id: &str,
        warehouse_id: &str,
        sku_id: &str,
        on_hand_qty: &str,
        as_of: &str,
        count_ref: &str,
    ) -> Result<(), String> {
        let (action, actor, source) = if restate {
            ("supply.position.restate", "ops:omar", "audit")
        } else {
            ("supply.position.open", "clerk:hana", "wms_scan")
        };
        self.ok(
            action,
            actor,
            json!({
                "snapshot_id": snapshot_id,
                "warehouse_id": warehouse_id,
                "sku_id": sku_id,
                "on_hand_qty": on_hand_qty,
                "unit": "each",
                "quantity_basis": "on_hand",
                "as_of": as_of,
                "count_source": source,
                "count_ref": count_ref,
            }),
        )
    }

    /// Ingest one clean baseline row per required feed (`pos_sales`, `store_counts`,
    /// `retail_receipts`, `unit_costs`) so durable cursors and watermarks are initialized.
    ///
    /// # Errors
    /// Returns an error if any baseline feed row fails ingestion or quarantines.
    pub fn ingest_baseline_feeds(
        &mut self,
        store_id: &str,
        sku_id: &str,
        unit_cost: &str,
        sold_at: &str,
        tag: &str,
    ) -> Result<(), String> {
        let rows =
            baseline_feed_payloads(&self.currency, store_id, sku_id, unit_cost, sold_at, tag);
        for (feed, dataset, source, event, record, payload) in &rows {
            let (_, clean) = self.ingest_feed(&FeedRow {
                feed_id: feed,
                mapping: feed,
                dataset,
                source,
                event,
                record,
                payload,
            })?;
            if !clean {
                return Err(format!(
                    "baseline feed {feed}/{record} quarantined unexpectedly"
                ));
            }
        }
        Ok(())
    }
}

type BaselineFeedRow = (
    &'static str,
    &'static str,
    &'static str,
    String,
    String,
    String,
);

fn baseline_feed_payloads(
    ccy: &str,
    store_id: &str,
    sku_id: &str,
    unit_cost: &str,
    sold_at: &str,
    tag: &str,
) -> [BaselineFeedRow; 4] {
    [
        (
            POS_FEED_ID,
            "pos_sale_line",
            "synthetic-pos",
            format!("ev-pos-base-{tag}"),
            format!("POS-BASE-{tag}"),
            json!({
                "pos_line_id": format!("POS-BASE-{tag}"), "branch_code": store_id,
                "barcode_sku": sku_id, "qty": 2, "uom": "PCS",
                "net_amount": 400, "ccy": ccy, "sold_at": sold_at,
            })
            .to_string(),
        ),
        (
            COUNTS_FEED_ID,
            "store_count",
            "synthetic-store-count",
            format!("ev-cnt-base-{tag}"),
            format!("CNT-BASE-{tag}"),
            json!({
                "count_id": format!("CNT-BASE-{tag}"), "store_code": store_id,
                "sku_code": sku_id, "counted_qty": 20, "uom": "PCS",
                "basis": "on_hand", "counted_at": sold_at,
            })
            .to_string(),
        ),
        (
            RECEIPTS_FEED_ID,
            "retail_receipt",
            "synthetic-retail-receipt",
            format!("ev-rcpt-base-{tag}"),
            format!("RCPT-BASE-{tag}"),
            json!({
                "receipt_line_id": format!("RCPT-BASE-{tag}"), "store_code": store_id,
                "sku_code": sku_id, "vendor_code": "ATELIER-01", "received_qty": 25,
                "damaged_qty": 0, "uom": "PCS", "unit_cost": unit_cost,
                "ccy": ccy, "received_at": sold_at,
            })
            .to_string(),
        ),
        (
            COSTS_FEED_ID,
            "unit_cost",
            "synthetic-cost",
            format!("ev-cost-base-{tag}"),
            format!("COST-BASE-{tag}"),
            json!({
                "cost_id": format!("COST-BASE-{tag}"), "sku_code": sku_id,
                "vendor_code": "ATELIER-01", "unit_cost": unit_cost,
                "ccy": ccy, "effective_from": "2026-07-01T00:00:00Z",
            })
            .to_string(),
        ),
    ]
}
