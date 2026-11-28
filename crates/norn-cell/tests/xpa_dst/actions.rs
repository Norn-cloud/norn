//! Domain action helpers for [`super::env::EntityCell`] over
//! `registry/packs/supply-lab/` omitting kernel-derived fields (`ORIGIN-DATAFLOW-1`).

use norn_cell::export::ExportRecord;
use serde_json::json;

use super::cuts::cell_id_for_entity;
use super::env::{
    COSTS_FEED_ID, COUNTS_FEED_ID, DEFAULT_CLOCK_MS, EntityCell, FeedRow, POS_FEED_ID,
    RECEIPTS_FEED_ID,
};
use super::prng::SECONDARY_SEED;

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
        let entity_id = self.entity_id.clone();
        let currency = self.currency.clone();
        self.ok(
            "supply.stock_lot.record",
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
                "cost_status": "costed",
                "unit_cost": unit_cost,
                "currency": currency,
            }),
        )
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
        let currency = self.currency.clone();
        self.ok(
            "supply.pos_sale.record",
            "clerk:hana",
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
                "currency": currency,
                "sold_at": sold_at,
            }),
        )
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
        let currency = self.currency.clone();
        self.ok(
            "supply.store_kpi.record",
            "planner:omar",
            json!({
                "rollup_id": kpi.rollup_id,
                "store_id": kpi.store_id,
                "sku_id": kpi.sku_id,
                "period_week": kpi.period_week,
                "unit": kpi.unit,
                "currency": currency,
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

/// Seeded two-book fixture (EGP sole proprietorship + USD partnership) with real
/// bilateral inter-entity transfer exports and an unrelated ledger-tip export.
pub struct TwoBookFixture {
    pub sole_egp_book: EntityCell,
    pub partner_usd_book: EntityCell,
    pub seller_transfer_export: ExportRecord,
    pub buyer_transfer_export: ExportRecord,
    pub unrelated_tip_export: ExportRecord,
}

/// Seed two isolated single-book cells (`LE-EGP`, `LE-USD`) including a real
/// bilateral inter-entity transfer (`SALE-USD-IE-1` <-> `LOT-EGP-IE-1`).
///
/// # Errors
/// Returns an error if any governed setup commit or single-book refusal fails.
pub fn seed_two_book_cells() -> Result<TwoBookFixture, String> {
    let (sole_egp_book, buyer_transfer_export, unrelated_tip_export) = seed_sole_egp_book()?;
    let (partner_usd_book, seller_transfer_export) = seed_partner_usd_book()?;
    Ok(TwoBookFixture {
        sole_egp_book,
        partner_usd_book,
        seller_transfer_export,
        buyer_transfer_export,
        unrelated_tip_export,
    })
}

fn seed_sole_egp_book() -> Result<(EntityCell, ExportRecord, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(SECONDARY_SEED, 0),
        "LE-EGP",
        "EGP",
        "org_group_egp",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Nile Sole Prop", "sole_proprietorship")?;
    let (second_book_code, _) = book.refused(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": "LE-ROGUE", "legal_name": "Rogue Second Book",
            "entity_kind": "corporation", "book_code": "BOOK-ROGUE-EGP",
            "functional_currency": "EGP",
        }),
    )?;
    if second_book_code != "action.precondition_failed" {
        return Err(format!(
            "second LegalEntity in one cell must refuse, got {second_book_code}"
        ));
    }
    book.register_sku("SKU-DRESS", "dry_goods", "each")?;
    book.register_sku("SKU-RING-IE", "dry_goods", "each")?;
    book.register_store(
        "ST-EGP-1",
        "ST-EGP-1",
        "CUST-CFC",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-DRESS",
        "COST-1",
        "SUP-1",
        "100",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-EGP-BASE",
        "ST-EGP-1",
        "SKU-DRESS",
        "CUST-CFC",
        "20",
        "each",
        "100",
        "2026-08-10T09:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-EGP-1",
        "ST-EGP-1",
        "SKU-DRESS",
        "4",
        "each",
        "2000",
        "100",
        "COST-1",
        "2026-09-02T12:00:00Z",
    )?;
    // Buyer leg: 2 units * 5000 EGP = 10,000 EGP (= 200 USD * 50).
    book.record_costed_lot(
        "LOT-EGP-IE-1",
        "ST-EGP-1",
        "SKU-RING-IE",
        "CUST-CFC",
        "2",
        "each",
        "5000",
        "2026-09-02T13:00:00Z",
    )?;
    let buyer_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-EGP-1",
        "SKU-DRESS",
        "100",
        "2026-09-02T12:00:00Z",
        "ST-EGP-1",
    )?;
    book.refresh_watermarks();
    let unrelated_tip = book.export_main_record(book.head()?.commits)?;
    Ok((book, buyer_export, unrelated_tip))
}

fn seed_partner_usd_book() -> Result<(EntityCell, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(SECONDARY_SEED, 1),
        "LE-USD",
        "USD",
        "org_group_usd",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Delta Partnership", "partnership")?;
    book.register_sku("SKU-RING", "dry_goods", "each")?;
    book.register_store(
        "ST-USD-1",
        "ST-USD-1",
        "CUST-CFC",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost("SKU-RING", "COST-1", "SUP-1", "80", "2026-07-01T00:00:00Z")?;
    book.record_costed_lot(
        "LOT-USD-BASE",
        "ST-USD-1",
        "SKU-RING",
        "CUST-CFC",
        "20",
        "each",
        "80",
        "2026-08-10T09:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-USD-EXT",
        "ST-USD-1",
        "SKU-RING",
        "3",
        "each",
        "800",
        "80",
        "COST-1",
        "2026-09-02T11:00:00Z",
    )?;
    // Seller leg: 2 units, 200 USD net revenue (total USD = 1000).
    book.record_costed_sale(
        "SALE-USD-IE-1",
        "ST-USD-1",
        "SKU-RING",
        "2",
        "each",
        "200",
        "80",
        "COST-1",
        "2026-09-02T12:30:00Z",
    )?;
    let seller_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-USD-1",
        "SKU-RING",
        "80",
        "2026-09-02T12:00:00Z",
        "ST-USD-1",
    )?;
    book.refresh_watermarks();
    Ok((book, seller_export))
}

/// Seeded Egyptian family-business fixture (sole proprietorship in EGP +
/// partnership in USD sharing `MALL-CFC` custodian) with real bilateral
/// inter-entity transfer exports (`SALE-IE-CFC-TRIM-01` <-> `LOT-IE-CFC-TRIM-01`).
pub struct EgyptianFamilyFixture {
    pub sole_book: EntityCell,
    pub partner_book: EntityCell,
    pub partner_sale_export: ExportRecord,
    pub sole_buyer_lot_export: ExportRecord,
    pub sole_unrelated_tip_export: ExportRecord,
}

/// Seed the Egyptian family-business multi-entity cells and bilateral transfer commits.
///
/// # Errors
/// Returns an error if any governed setup commit fails.
pub fn seed_egyptian_family_fixture(seed: u64) -> Result<EgyptianFamilyFixture, String> {
    let (sole_book, sole_buyer_lot_export, sole_unrelated_tip_export) =
        seed_egyptian_sole_book(seed)?;
    let (partner_book, partner_sale_export) = seed_egyptian_partner_book(seed)?;
    Ok(EgyptianFamilyFixture {
        sole_book,
        partner_book,
        partner_sale_export,
        sole_buyer_lot_export,
        sole_unrelated_tip_export,
    })
}

fn seed_egyptian_sole_book(seed: u64) -> Result<(EntityCell, ExportRecord, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(seed, 51),
        "LE-NILE-SOLE",
        "EGP",
        "org_family_nile_sole",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Nile Apparel Sole Proprietorship", "sole_proprietorship")?;
    book.register_sku("SKU-ABAYA", "dry_goods", "each")?;
    book.register_sku("SKU-GOLD-TRIM", "spares", "each")?;
    book.register_store(
        "ST-SOLE-CFC",
        "Nile CFC Booth",
        "CUST-MALL-CFC-OPS",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-ABAYA",
        "COST-ABAYA-1",
        "SUP-ATELIER",
        "400",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-SOLE-1",
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "CUST-MALL-CFC-OPS",
        "100",
        "each",
        "400",
        "2026-08-03T09:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-SOLE-1",
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "30",
        "each",
        "27000",
        "400",
        "COST-ABAYA-1",
        "2026-09-02T14:00:00Z",
    )?;
    // Buyer leg: 2 units * 7275 EGP = 14,550 EGP (= 300 USD * 48.5).
    book.record_costed_lot(
        "LOT-IE-CFC-TRIM-01",
        "ST-SOLE-CFC",
        "SKU-GOLD-TRIM",
        "CUST-MALL-CFC-OPS",
        "2",
        "each",
        "7275",
        "2026-09-02T15:30:00Z",
    )?;
    let buyer_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "400",
        "2026-09-02T14:00:00Z",
        "egypt-sole",
    )?;
    book.refresh_watermarks();
    let unrelated_tip = book.export_main_record(book.head()?.commits)?;
    Ok((book, buyer_export, unrelated_tip))
}

fn seed_egyptian_partner_book(seed: u64) -> Result<(EntityCell, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(seed, 52),
        "LE-CAIRO-PARTNER",
        "USD",
        "org_family_cairo_partner",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Cairo Atelier Partnership", "partnership")?;
    book.register_sku("SKU-GOLD-TRIM", "spares", "each")?;
    book.register_store(
        "ST-PART-CFC",
        "Cairo CFC Gallery",
        "CUST-MALL-CFC-OPS",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-GOLD-TRIM",
        "COST-TRIM-1",
        "SUP-ATELIER",
        "80",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-PART-1",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "CUST-MALL-CFC-OPS",
        "40",
        "each",
        "80",
        "2026-08-03T09:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-PART-EXT",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "8",
        "each",
        "1200",
        "80",
        "COST-TRIM-1",
        "2026-09-02T14:30:00Z",
    )?;
    // Seller leg: 2 units, 300 USD net revenue (total USD = 1500).
    book.record_costed_sale(
        "SALE-IE-CFC-TRIM-01",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "2",
        "each",
        "300",
        "80",
        "COST-TRIM-1",
        "2026-09-02T15:00:00Z",
    )?;
    let seller_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "80",
        "2026-09-02T15:00:00Z",
        "egypt-partner",
    )?;
    book.refresh_watermarks();
    Ok((book, seller_export))
}
