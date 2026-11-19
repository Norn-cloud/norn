#!/usr/bin/env python3
"""Generate distributor-1's source schemas and mappings (TypeScript) from one spec.

Two tracks, so the reuse number is measured, not claimed:

* pack track: registry/packs/{erpnext,odoo,shopify}/records.ts re-bound to this
  deployment. Their `maps` are copied byte for byte; only the source block
  (id, system, raw metadata, code sets this distributor needs) is rewritten.
  Saleor is bound to the Shopify pack: the same commerce shapes.
* new track: SPEC below, one entry per source dataset, every declared field
  either mapped or ignored with a reason (MAP-TOTAL: the generator refuses a
  field that is neither).

A mapping pins its source schema by envelope hash. The hash is only known after
compiling, so this writes a placeholder; pin.ts fills it from the checker's
SOURCE_HASH_MISMATCH diagnostic. Output: OUT_DIR/*.ts and OUT_DIR/coverage.json.

Usage: gen_mappings.py OUT_DIR
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
PLACEHOLDER = "0" * 64
UNITS = [{"value": "{each}"}, {"value": "kg"}]  # canon has no L, box or carton (core.units)

# ------------------------------------------------------------------ pack track
PACK_REBIND = {
    # system: (pack dir, deployment source id, system label, code-set overrides by id)
    "erpnext": ("erpnext", "d1.erpnext.source", "erpnext", {
        "holding_unit": UNITS, "order_unit": UNITS, "delivery_unit": UNITS,
        "holding_valuation_method": [{"value": "fifo"}, {"value": "moving_average"}],
        "posting_account_role": [{"value": "cogs"}, {"value": "inventory"}, {"value": "receivable"}, {"value": "income"}, {"value": "cash"}, {"value": "tax"}],
        "posting_book": [{"value": "d1/books-egp"}],
    }),
    "odoo": ("odoo", "d1.odoo.source", "odoo", {
        "holding_unit": UNITS, "order_unit": UNITS, "delivery_unit": UNITS,
        "holding_valuation_method": [{"value": "fifo"}, {"value": "moving_average"}],
        "posting_account_role": [{"value": "cogs"}, {"value": "inventory"}, {"value": "receivable"}, {"value": "income"}, {"value": "cash"}, {"value": "tax"}],
        "posting_book": [{"value": "d1/books-egp"}],
    }),
    "saleor": ("shopify", "d1.saleor.source", "saleor", {
        "holding_unit": UNITS, "order_unit": UNITS, "delivery_unit": UNITS,
    }),
}


def _ts(v) -> str:
    return json.dumps(v, ensure_ascii=False)


def rebind_pack(system: str) -> tuple[str, dict]:
    pack, source_id, label, overrides = PACK_REBIND[system]
    text = (REPO / "registry/packs" / pack / "records.ts").read_text()
    head, sep, maps_onward = text.partition("  maps: [")
    assert sep, f"{pack}: no maps block"
    old_source_id = re.search(r'id: "([a-z]+\.three\.source)"', head).group(1)
    old_mapping_id = re.search(r'id: "([a-z]+\.three\.mapping)"', head).group(1)
    head = head.replace("../../core/src/physics.ts", "../../../../../../registry/core/src/physics.ts")
    head = head.replace("../canon/records.ts", "../../../../../../registry/packs/canon/records.ts")
    head = head.replace(f'id: "{old_source_id}"', f'id: "{source_id}"').replace(f'schemaId: "{old_source_id}"', f'schemaId: "{source_id}"')
    head = head.replace(f'id: "{old_mapping_id}"', f'id: "{source_id.replace(".source", ".mapping")}"')
    head = re.sub(r'source: \{system: "[^"]+", instance: "demo", version: "[^"]+"\}', f'source: {{system: "{label}", instance: "distributor-1", version: "{label}-d1/1"}}', head)
    head = re.sub(r'adapter: \{name: "synthetic-record-replay"', 'adapter: {name: "d1-extract"', head)
    head = re.sub(r'rawMetadata: \{uri: "[^"]+", sha256: "[0-9a-f]+"\}', f'rawMetadata: {{uri: "fixtures/deployments/distributor-1/extract/{system}.py", sha256: "{PLACEHOLDER}"}}', head)
    head = re.sub(r'(source: \{schemaId: "[^"]+", version: "[^"]+", sha256: ")[0-9a-f]+', rf"\g<1>{PLACEHOLDER}", head)
    changed_sets = 0
    for set_id, values in overrides.items():
        pat = re.compile(r'(\{id: "' + re.escape(set_id) + r'", dataset: "[^"]+", field: "[^"]+", origin: "declared", values: )(\[[^\]]*\]|ISO_4217_CODE_SET)(\})')
        head, n = pat.subn(lambda m: m.group(1) + _ts(values) + m.group(3), head)
        changed_sets += n
    # `'erpnext/demo/...'` provenance prefixes inside maps are data, so the maps stay byte-identical.
    maps_text = "  maps: [" + maps_onward
    stats = {"track": "pack", "pack": pack, "maps": maps_text.count("      id: \""), "mapLines": maps_text.count("\n"),
             "mapLinesChanged": 0, "sourceLines": head.count("\n"), "codeSetsOverridden": changed_sets,
             "fields": len(re.findall(r"\{dataset: \"", head))}
    return head + maps_text, stats


# ------------------------------------------------------------------- new track
# dataset: {fields: [(name, type)], maps: [...]}; type s|d|b, trailing ? = optional.
# Every field must appear in a map's `uses` or in `ignore` (MAP-TOTAL).
SPEC: dict[str, dict] = {
    "erpnext_docs": {
        "system": "erpnext", "imports": ["Party", "Item"],
        "datasets": {
            "Customer": {"fields": [("name", "s"), ("x_norn_ref", "s"), ("customer_name", "s"), ("territory", "s"), ("tax_id", "s?"), ("credit_limit", "d?"), ("customer_group", "s"), ("default_currency", "s?"), ("modified", "s")],
                         "ignore": {"customer_group": "A pricing segment, not a party property the canon or the profile carries.", "default_currency": "Billing default only; each document states its own currency.", "modified": "Change stamp; the profile has no valid time."}},
            "Sales Invoice": {"fields": [("name", "s"), ("customer_ref", "s"), ("posting_date", "s"), ("posting_time", "s"), ("currency", "s"), ("grand_total", "d"), ("territory", "s"), ("docstatus", "s")],
                              "ignore": {"posting_time": "Joined into posted_at by the extractor (ERPNext splits date and time).", "docstatus": "Only submitted (1) invoices are exported; the code set refuses the rest."}},
            "Sales Invoice Item": {"fields": [("name", "s"), ("parent", "s"), ("item_ref", "s?"), ("qty", "d"), ("rate", "d"), ("amount", "d"), ("currency", "s")], "ignore": {}},
            "Stock Ledger": {"fields": [("name", "s"), ("item_ref", "s"), ("warehouse", "s"), ("opening_qty", "d"), ("closing_qty", "d"), ("as_of", "s")], "ignore": {}},
            "Stock Ledger Entry": {"fields": [("name", "s"), ("ledger", "s"), ("actual_qty", "d"), ("posting_datetime", "s"), ("voucher_no", "s"), ("voucher_type", "s")],
                                   "ignore": {"voucher_type": "Kept in the reference text; movements are typed by sign only."}},
        },
        "codeSets": [("currency", "Sales Invoice", "currency", ["EGP", "USD"]), ("line_currency", "Sales Invoice Item", "currency", ["EGP", "USD"]), ("docstatus", "Sales Invoice", "docstatus", ["1"]),
                     ("territory", "Sales Invoice", "territory", ["Cairo", "Alexandria", "Giza", "Delta", "Upper Egypt"]), ("cust_territory", "Customer", "territory", ["Cairo", "Alexandria", "Giza", "Delta", "Upper Egypt"])],
        "maps": [
            {"id": "profile", "dataset": "Customer", "type": "CustomerProfile", "subject": "'erpnext/' + input.name", "uses": ["name", "x_norn_ref", "customer_name", "territory", "tax_id", "credit_limit"],
             "props": {"profile_id": "'erpnext/' + input.name", "region": "input.territory", "tax_id": "input.tax_id", "credit_limit": "input.credit_limit", "name_local": "input.customer_name", "source_system": "'erpnext'"},
             "links": [("party", "party", "input.x_norn_ref")]},
            {"id": "party", "dataset": "Customer", "type": "Party", "iface": "Agent", "subject": "input.x_norn_ref", "uses": ["x_norn_ref", "customer_name"],
             "props": {"display_name": "input.customer_name", "kind": "'organization'"}},
            {"id": "invoice", "dataset": "Sales Invoice", "type": "SalesInvoice", "iface": "ItemizedDocument", "subject": "'erpnext/' + input.name", "uses": ["name", "customer_ref", "posting_date", "currency", "grand_total", "territory"],
             "props": {"invoice_id": "'erpnext/' + input.name", "posted_at": "timestamp(input.posting_date)", "region": "input.territory", "currency": "input.currency", "total": "input.grand_total", "source_system": "'erpnext'"},
             "links": [("customer", "party", "input.customer_ref")]},
            {"id": "line", "dataset": "Sales Invoice Item", "type": "DocumentLine", "iface": "LineItem", "subject": "'erpnext/' + input.name", "uses": ["name", "parent", "item_ref", "qty", "rate", "amount", "currency"],
             "props": {"line_id": "'erpnext/' + input.name", "quantity": "input.qty", "rate": "input.rate", "amount": "input.amount", "currency": "input.currency"},
             "links": [("document", "invoice", "'erpnext/' + input.parent")], "externalLinks": [("item", "input.item_ref")]},
            {"id": "ledger", "dataset": "Stock Ledger", "type": "StockLedger", "subject": "'erpnext/' + input.name", "uses": ["name", "item_ref", "warehouse", "opening_qty", "closing_qty", "as_of"],
             "props": {"ledger_id": "'erpnext/' + input.name", "location": "input.warehouse", "opening_qty": "input.opening_qty", "closing_qty": "input.closing_qty", "observed_at": "timestamp(input.as_of)", "source_system": "'erpnext'"},
             "externalLinks": [("item", "input.item_ref")]},
            {"id": "move", "dataset": "Stock Ledger Entry", "type": "StockMove", "subject": "'erpnext/' + input.name", "uses": ["name", "ledger", "actual_qty", "posting_datetime", "voucher_no"],
             "props": {"move_id": "'erpnext/' + input.name", "quantity": "input.actual_qty", "occurred_at": "timestamp(input.posting_datetime)", "reference": "input.voucher_no"},
             "links": [("ledger", "ledger", "'erpnext/' + input.ledger")]},
        ],
    },
    "odoo_docs": {
        "system": "odoo", "imports": ["Party", "Item"],
        "datasets": {
            "res.partner": {"fields": [("id", "s"), ("x_norn_ref", "s"), ("name", "s"), ("state", "s"), ("vat", "s?"), ("credit_limit", "d?"), ("lang", "s"), ("write_date", "s")],
                            "ignore": {"lang": "Display language of the partner's documents.", "write_date": "Change stamp; the profile has no valid time."}},
            "account.move": {"fields": [("id", "s"), ("name", "s"), ("partner_ref", "s"), ("invoice_date", "s"), ("currency", "s"), ("amount_total", "d"), ("region", "s"), ("state", "s"), ("move_type", "s")],
                             "ignore": {"state": "Only posted invoices are exported; the code set refuses the rest.", "move_type": "Only out_invoice is exported (code set)."}},
            "account.move.line": {"fields": [("id", "s"), ("move_id", "s"), ("item_ref", "s?"), ("quantity", "d"), ("price_unit", "d"), ("price_subtotal", "d"), ("currency", "s")], "ignore": {}},
        },
        "codeSets": [("currency", "account.move", "currency", ["EGP", "USD"]), ("line_currency", "account.move.line", "currency", ["EGP", "USD"]), ("state", "account.move", "state", ["posted"]),
                     ("move_type", "account.move", "move_type", ["out_invoice"]), ("region", "account.move", "region", ["Cairo", "Alexandria", "Giza", "Delta", "Upper Egypt"]),
                     ("partner_state", "res.partner", "state", ["Cairo", "Alexandria", "Giza", "Delta", "Upper Egypt"])],
        "maps": [
            {"id": "profile", "dataset": "res.partner", "type": "CustomerProfile", "subject": "'odoo/' + input.id", "uses": ["id", "x_norn_ref", "name", "state", "vat", "credit_limit"],
             "props": {"profile_id": "'odoo/' + input.id", "region": "input.state", "tax_id": "input.vat", "credit_limit": "input.credit_limit", "name_local": "input.name", "source_system": "'odoo'"},
             "links": [("party", "party", "input.x_norn_ref")]},
            {"id": "party", "dataset": "res.partner", "type": "Party", "iface": "Agent", "subject": "input.x_norn_ref", "uses": ["x_norn_ref", "name"],
             "props": {"display_name": "input.name", "kind": "'organization'"}},
            {"id": "invoice", "dataset": "account.move", "type": "SalesInvoice", "iface": "ItemizedDocument", "subject": "'odoo/' + input.name", "uses": ["id", "name", "partner_ref", "invoice_date", "currency", "amount_total", "region"],
             "props": {"invoice_id": "'odoo/' + input.name", "posted_at": "timestamp(input.invoice_date)", "region": "input.region", "currency": "input.currency", "total": "input.amount_total", "source_system": "'odoo'"},
             "links": [("customer", "party", "input.partner_ref")]},
            {"id": "line", "dataset": "account.move.line", "type": "DocumentLine", "iface": "LineItem", "subject": "'odoo/' + input.id", "uses": ["id", "move_id", "item_ref", "quantity", "price_unit", "price_subtotal", "currency"],
             "props": {"line_id": "'odoo/' + input.id", "quantity": "input.quantity", "rate": "input.price_unit", "amount": "input.price_subtotal", "currency": "input.currency"},
             "links": [("document", "invoice", "'odoo/' + input.move_id")], "externalLinks": [("item", "input.item_ref")]},
        ],
    },
    "saleor_docs": {
        "system": "saleor", "imports": ["Party", "Item"],
        "datasets": {
            "Order": {"fields": [("id", "s"), ("number", "s"), ("user_ref", "s"), ("created", "s"), ("currency", "s"), ("total_gross", "d"), ("region", "s"), ("status", "s")],
                      "ignore": {"status": "Only fulfilled orders are exported (code set); an order is revenue once fulfilled in this deployment."}},
            "OrderLine": {"fields": [("id", "s"), ("order_id", "s"), ("item_ref", "s?"), ("quantity", "d"), ("unit_price_gross", "d"), ("total_price_gross", "d"), ("currency", "s")], "ignore": {}},
        },
        "codeSets": [("currency", "Order", "currency", ["EGP", "USD"]), ("line_currency", "OrderLine", "currency", ["EGP", "USD"]), ("status", "Order", "status", ["FULFILLED", "PARTIALLY_FULFILLED"]),
                     ("region", "Order", "region", ["Cairo", "Alexandria", "Giza", "Delta", "Upper Egypt"])],
        "maps": [
            {"id": "party", "dataset": "Order", "type": "Party", "iface": "Agent", "subject": "input.user_ref", "uses": ["user_ref"], "props": {"display_name": "input.user_ref", "kind": "'organization'"}},
            {"id": "invoice", "dataset": "Order", "type": "SalesInvoice", "iface": "ItemizedDocument", "subject": "'saleor/' + input.number", "uses": ["id", "number", "user_ref", "created", "currency", "total_gross", "region"],
             "props": {"invoice_id": "'saleor/' + input.number", "posted_at": "timestamp(input.created)", "region": "input.region", "currency": "input.currency", "total": "input.total_gross", "source_system": "'saleor'"},
             "links": [("customer", "party", "input.user_ref")]},
            {"id": "line", "dataset": "OrderLine", "type": "DocumentLine", "iface": "LineItem", "subject": "'saleor/' + input.id", "uses": ["id", "order_id", "item_ref", "quantity", "unit_price_gross", "total_price_gross", "currency"],
             "props": {"line_id": "'saleor/' + input.id", "quantity": "input.quantity", "rate": "input.unit_price_gross", "amount": "input.total_price_gross", "currency": "input.currency"},
             "links": [("document", "invoice", "'saleor/' + input.order_id")], "externalLinks": [("item", "input.item_ref")]},
        ],
    },
    "legacy": {
        "system": "legacy-wms", "imports": ["Item", "Holding", "Party"],
        "datasets": {
            "wms.itm_mst": {"fields": [("id", "s"), ("itm_cd", "s"), ("x_norn_ref", "s"), ("itm_desc_ar", "s"), ("uom", "s"), ("std_cost_txt", "s"), ("actv", "s")],
                            "ignore": {"id": "Recorded-export row identity (= itm_cd).", "uom": "Packaging text (Box/Carton/Litre) with no canon unit; holdings carry the unit.", "std_cost_txt": "Text money with no currency; not a valuation.", "actv": "Active flag; every exported item is active (code set)."}},
            "wms.stk_bal": {"fields": [("id", "s"), ("x_norn_ref", "s"), ("itm_cd", "s"), ("item_ref", "s"), ("whs_cd", "s"), ("qty", "d"), ("qty_txt", "s"), ("unit", "s"), ("upd_dt", "s"), ("owner_ref", "s")],
                            "ignore": {"qty_txt": "Raw text ('1,234'); the extractor's exact decimal is mapped as qty (ZEN has no number parser)."}},
            "wms.ledger": {"fields": [("id", "s"), ("item_ref", "s"), ("whs_cd", "s"), ("opening_qty", "d"), ("closing_qty", "d"), ("upd_dt", "s")], "ignore": {}},
            "wms.stk_txn": {"fields": [("id", "s"), ("txn_id", "s"), ("ledger", "s"), ("qty", "d"), ("qty_txt", "s"), ("txn_dt", "s"), ("ref_no", "s")],
                            "ignore": {"id": "Recorded-export row identity (= txn_id).", "qty_txt": "Raw text; the exact decimal is mapped as qty."}},
        },
        "codeSets": [("actv", "wms.itm_mst", "actv", ["Y"]), ("unit", "wms.stk_bal", "unit", ["{each}", "kg"])],
        "maps": [
            {"id": "item", "dataset": "wms.itm_mst", "type": "Item", "iface": "Catalog", "subject": "input.x_norn_ref", "uses": ["itm_cd", "x_norn_ref", "itm_desc_ar"],
             "props": {"id": "input.x_norn_ref", "code": "input.itm_cd", "label": "input.itm_desc_ar"}},
            {"id": "holding", "dataset": "wms.stk_bal", "type": "Holding", "iface": "Resource", "subject": "input.x_norn_ref", "uses": ["id", "x_norn_ref", "itm_cd", "item_ref", "whs_cd", "qty", "unit", "upd_dt", "owner_ref"],
             # upd_dt has no zone: timestamp() of a zone-less text is the experiment
             "props": {"resource_code": "input.itm_cd", "quantity": "input.qty", "observed_at": "timestamp(input.upd_dt)", "unit": "input.unit", "currency": "null", "valuation_method": "null",
                       "location": "input.whs_cd", "quantity_basis": "'on_hand'", "stock_cost": "null", "cost_currency": "null"},
             "links": [("item", "item", "input.item_ref"), ("owner", None, "input.owner_ref")],
             "gaps": [("custodian", "not_in_source", "The WMS records no custodian."), ("stock_cost", "not_in_source", "No valuation in the WMS.")]},
            {"id": "ledger", "dataset": "wms.ledger", "type": "StockLedger", "subject": "'legacy/' + input.id", "uses": ["id", "item_ref", "whs_cd", "opening_qty", "closing_qty", "upd_dt"],
             "props": {"ledger_id": "'legacy/' + input.id", "location": "input.whs_cd", "opening_qty": "input.opening_qty", "closing_qty": "input.closing_qty", "observed_at": "timestamp(input.upd_dt)", "source_system": "'legacy-wms'"},
             "links": [("item", "item", "input.item_ref")]},
            {"id": "move", "dataset": "wms.stk_txn", "type": "StockMove", "subject": "'legacy/' + input.txn_id", "uses": ["txn_id", "ledger", "qty", "txn_dt", "ref_no"],
             "props": {"move_id": "'legacy/' + input.txn_id", "quantity": "input.qty", "occurred_at": "timestamp(input.txn_dt)", "reference": "input.ref_no"},
             "links": [("ledger", "ledger", "'legacy/' + input.ledger")]},
        ],
    },
    "files": {
        "system": "files", "imports": ["Party", "Item"],
        "datasets": {
            "bank.receipt": {"fields": [("id", "s"), ("bank", "s"), ("format", "s"), ("booked_at", "s"), ("currency", "s"), ("amount", "d"), ("remittance", "s"), ("invoice_hint", "s?"), ("counterparty_text", "s?")],
                             "ignore": {"format": "mt940 or camt.053: provenance, kept in the source record id.", "counterparty_text": "Free-text payer name; never establishes identity (no fuzzy party matching).",
                                        "invoice_hint": "A candidate match parsed from remittance text; the match itself is the governed action distributor.receipt.reconcile (a mapping cannot link across sources)."}},
            "edi.850": {"fields": [("id", "s"), ("po_number", "s"), ("buyer_ref", "s?"), ("po_date", "s"), ("stated_total", "d"), ("currency", "s")], "ignore": {}},
            "edi.850.line": {"fields": [("id", "s"), ("po_id", "s"), ("item_ref", "s?"), ("qty", "d"), ("price", "d"), ("amount", "d"), ("currency", "s")], "ignore": {}},
            "pricelist": {"fields": [("id", "s"), ("item_ref", "s"), ("code", "s"), ("name_ar", "s"), ("list_price", "d"), ("list_price_text", "s"), ("web_price", "d?"), ("web_price_text", "s"), ("group", "s")],
                          "ignore": {"code": "Alias; the crosswalk resolved it to item_ref.", "name_ar": "Arabic label; canon.Item has one label (ERPNext's English one wins).", "list_price_text": "Raw 'EGP 1,234.50' text; the extractor's exact decimal is list_price.",
                                     "web_price_text": "Raw '1234,50' (decimal comma) text; the exact decimal is web_price.", "group": "Item group: no canon home (canon.Item has id, code, label)."}},
            "aging": {"fields": [("id", "s"), ("x_norn_ref", "s"), ("name_ar", "s"), ("region_ar", "s"), ("balance", "d"), ("last_txn", "s"), ("blocked", "b")],
                      "ignore": {"region_ar": "Arabic region text; the region comes from the ERP customer profile."}},
        },
        "codeSets": [("bank", "bank.receipt", "bank", ["BANK-A", "BANK-B"]), ("rcurrency", "bank.receipt", "currency", ["EGP", "USD"]), ("pocurrency", "edi.850", "currency", ["EGP", "USD"]), ("polcurrency", "edi.850.line", "currency", ["EGP", "USD"])],
        "maps": [
            {"id": "receipt", "dataset": "bank.receipt", "type": "BankReceipt", "subject": "'bank/' + input.id", "uses": ["id", "bank", "booked_at", "currency", "amount", "remittance"],
             "props": {"receipt_id": "'bank/' + input.id", "bank": "input.bank", "booked_at": "timestamp(input.booked_at)", "currency": "input.currency", "amount": "input.amount", "remittance": "input.remittance", "matched": "false"},
             "externalLinks": [("invoice", "")]},
            {"id": "po", "dataset": "edi.850", "type": "CustomerOrder", "iface": "ItemizedDocument", "subject": "'edi/' + input.po_number", "uses": ["id", "po_number", "buyer_ref", "po_date", "stated_total", "currency"],
             "props": {"order_id": "'edi/' + input.po_number", "ordered_at": "timestamp(input.po_date)", "currency": "input.currency", "total": "input.stated_total", "source_system": "'edi-x12'"},
             "externalLinks": [("customer", "input.buyer_ref")]},
            {"id": "po_line", "dataset": "edi.850.line", "type": "DocumentLine", "iface": "LineItem", "subject": "'edi/' + input.id", "uses": ["id", "po_id", "item_ref", "qty", "price", "amount", "currency"],
             "props": {"line_id": "'edi/' + input.id", "quantity": "input.qty", "rate": "input.price", "amount": "input.amount", "currency": "input.currency"},
             "links": [("document", "po", "'edi/' + input.po_id")], "externalLinks": [("item", "input.item_ref")]},
            {"id": "price", "dataset": "pricelist", "type": "PriceListEntry", "subject": "'xlsx/' + input.id", "uses": ["id", "item_ref", "list_price", "web_price"],
             "props": {"entry_id": "'xlsx/' + input.id", "list_price": "input.list_price", "web_price": "input.web_price", "currency": "'EGP'"},
             "externalLinks": [("item", "input.item_ref")]},
            {"id": "party", "dataset": "aging", "type": "Party", "iface": "Agent", "subject": "input.x_norn_ref", "uses": ["x_norn_ref", "name_ar"], "props": {"display_name": "input.name_ar", "kind": "'organization'"}},
            {"id": "ar", "dataset": "aging", "type": "ReceivableBalance", "subject": "'aging/' + input.id", "uses": ["id", "x_norn_ref", "balance", "last_txn", "blocked"],
             "props": {"balance_id": "'aging/' + input.id", "balance": "input.balance", "last_txn_on": "input.last_txn", "blocked": "input.blocked"},
             "links": [("party", "party", "input.x_norn_ref")]},
        ],
    },
}

SPEC["bpic"] = {
    "system": "sap-p2p", "imports": ["Party"],
    "datasets": {
        "vendor": {"fields": [("id", "s"), ("name", "s")], "ignore": {}},
        "case": {"fields": [("id", "s"), ("vendor", "s"), ("company", "s"), ("doc_type", "s"), ("item_type", "s"), ("item_category", "s"), ("spend_area", "s"), ("sub_spend_area", "s"),
                            ("spend_class", "s"), ("source", "s"), ("gr_based", "b"), ("goods_receipt", "b"), ("purch_doc", "s"), ("item", "s")],
                 "ignore": {"sub_spend_area": "Second spend level; the case keeps the first.", "spend_class": "Spend classification text duplicates the spend area for this log.",
                            "source": "Constant 'sourceSystemID_0000' in BPIC 2019.", "purch_doc": "Part of the case id (document + item).", "item": "Part of the case id (document + item)."}},
        "event": {"fields": [("id", "s"), ("case", "s"), ("activity", "s"), ("at", "s"), ("user", "s"), ("org_resource", "s"), ("net_worth", "d")],
                  "ignore": {"org_resource": "Always 'NONE' in BPIC 2019."}},
    },
    "codeSets": [],
    "maps": [
        {"id": "vendor", "dataset": "vendor", "type": "Party", "subject": "'bpic/vendor/' + input.id", "uses": ["id", "name"], "props": {"display_name": "input.name", "kind": "'organization'"}},
        {"id": "case", "dataset": "case", "type": "P2PCase", "subject": "input.id", "uses": ["id", "vendor", "company", "doc_type", "item_type", "item_category", "spend_area", "gr_based", "goods_receipt"],
         "props": {"case_id": "input.id", "company": "input.company", "document_type": "input.doc_type", "item_type": "input.item_type", "item_category": "input.item_category",
                   "spend_area": "input.spend_area", "gr_based_invoice": "input.gr_based", "goods_receipt": "input.goods_receipt"},
         "links": [("vendor", "vendor", "'bpic/vendor/' + input.vendor")]},
        {"id": "event", "dataset": "event", "type": "P2PEvent", "subject": "input.id", "uses": ["id", "case", "activity", "at", "user", "net_worth"],
         "props": {"event_id": "input.id", "activity": "input.activity", "occurred_at": "timestamp(input.at)", "user": "input.user", "net_worth_eur": "input.net_worth"},
         "links": [("case", "case", "input.case")]},
    ],
}

IFACE = {"Party": "Agent", "Item": "Catalog", "Holding": "Resource", "SalesInvoice": "ItemizedDocument", "CustomerOrder": "ItemizedDocument", "DocumentLine": "LineItem"}
CANON = {"Party", "Item", "Holding"}
TYPES = {"s": "string", "d": "decimal", "b": "boolean"}


def gen_module(key: str, spec: dict) -> tuple[str, dict]:
    sid, mid = f"d1.{key}.source", f"d1.{key}.mapping"
    fields, cov = [], {}
    for ds, d in spec["datasets"].items():
        used = set().union(*[set(m.get("uses", [])) for m in spec["maps"] if m["dataset"] == ds])
        for name, t in d["fields"]:
            if name not in used and name not in d["ignore"]:
                raise SystemExit(f"MAP-TOTAL: {key}/{ds}.{name} is neither mapped nor ignored")
            cov.setdefault(ds, {"mapped": 0, "ignored": 0})["mapped" if name in used else "ignored"] += 1
        for i, (name, t) in enumerate(d["fields"]):
            fields.append(f'    {{dataset: {_ts(ds)}, name: {_ts(name)}, ordinal: {i}, nativeType: "Text", typeCandidate: {{type: "{TYPES[t[0]]}", origin: "declared"}}, required: {"false" if t.endswith("?") else "true"}}}')
    code_sets = [f'    {{id: {_ts(i)}, dataset: {_ts(ds)}, field: {_ts(f)}, origin: "declared", values: {_ts([{"value": v} for v in vals])}}}' for i, ds, f, vals in spec["codeSets"]]
    pk = [f'    {{id: "pk_{n}", kind: "primary_key", dataset: {_ts(ds)}, fields: [{_ts(d["fields"][0][0])}], origin: "declared"}}' for n, (ds, d) in enumerate(spec["datasets"].items())]
    maps = []
    for m in spec["maps"]:
        if m.get("stub"):
            continue
        d = spec["datasets"][m["dataset"]]
        iface = IFACE.get(m["type"], "Observed")
        props = ", ".join(f'{{property: {_ts(p)}, value: {_ts(v)}}}' for p, v in m["props"].items())
        links = [f'{{link: {_ts(l)}, targetMap: {_ts(t)}, key: {_ts(k)}}}' for l, t, k in m.get("links", []) if t]
        ignored = [f'{{field: {_ts(f)}, reason: {_ts(r)}}}' for f, r in d["ignore"].items()]
        gaps = [f'{{member: {_ts(g)}, reason: {_ts(r)}, note: {_ts(n)}}}' for g, r, n in m.get("gaps", [])]
        # externalLinks name objects another mapping produces; MappingIR links only to maps of the same mapping,
        # so these are carried as gaps (deferred) and the cross-source link is a GAP the report counts.
        gaps += [f'{{member: {_ts(l)}, reason: "deferred", note: "Target is produced by another source mapping; MappingIR links only within one mapping."}}' for l, _ in m.get("externalLinks", [])]
        gaps += [f'{{member: {_ts(l)}, reason: "deferred", note: "Owner party comes from another source."}}' for l, t, _ in m.get("links", []) if not t]
        target = f'{{type: {m["type"]}, interface: {iface}}}'
        source_record = "'" + f'{spec["system"]}/d1/{m["dataset"]}/' + "' + input." + d["fields"][0][0]
        maps.append("    {\n" + ",\n".join(filter(None, [
            f'      id: {_ts(m["id"])}', f'      source: {{dataset: {_ts(m["dataset"])}}}', f"      target: {target}",
            f'      subject: {_ts(m["subject"])}', f"      sourceRecord: {_ts(source_record)}",
            f"      properties: [{props}]", f"      links: [{', '.join(links)}]" if links else "", f"      gaps: [{', '.join(gaps)}]" if gaps else "",
            f"      ignoredFields: [{', '.join(ignored)}]" if ignored and m is [x for x in spec["maps"] if x["dataset"] == m["dataset"] and not x.get("stub")][0] else "",
        ])) + "\n    }")
    used_types = sorted({m["type"] for m in spec["maps"] if not m.get("stub")})
    dist_types = [t for t in used_types if t not in CANON]
    canon_types = sorted({t for t in used_types if t in CANON})
    own = [t for t in dist_types if t not in IFACE]
    ifaces = sorted({IFACE[t] for t in used_types if t in IFACE} | ({"Observed"} if own else set()))
    src = f"""// GENERATED by gen_mappings.py from SPEC["{key}"]; edit the spec, not this file.
import {{ Norn }} from "@norn/authoring"
import {{ {", ".join(sorted({"Agent", "Resource"} & set(ifaces)) or ["Agent"])} }} from "../../../../../../registry/core/src/physics.ts"
import {{ {", ".join(sorted(set(canon_types) | ({"Catalog"} & set(ifaces)) | ({"ItemizedDocument", "LineItem"} & set(ifaces))))} }} from "../../../../../../registry/packs/canon/records.ts"
{f'import {{ {", ".join([t for t in dist_types if not t.startswith("P2P")] + (["Observed"] if own else []))} }} from "../distributor.ts"' if dist_types else ""}
{f'import {{ {", ".join(t for t in dist_types if t.startswith("P2P"))} }} from "../p2p.ts"' if any(t.startswith("P2P") for t in dist_types) else ""}
export const Source = Norn.sourceSchema({{
  id: "{sid}", title: "distributor-1 {key} extract", version: "0.1.0",
  source: {{system: "{spec["system"]}", instance: "distributor-1", version: "{spec["system"]}-d1/1"}},
  observedAt: "2026-11-01T00:00:00Z",
  adapter: {{name: "d1-extract", version: "0.1.0"}},
  rawMetadata: {{uri: "fixtures/deployments/distributor-1/extract/", sha256: "{PLACEHOLDER}"}},
  datasets: [
{",{}".format(chr(10)).join(f'    {{id: {_ts(ds)}, nativeName: {_ts(ds)}, kind: "document"}}' for ds in spec["datasets"])}
  ],
  fields: [
{("," + chr(10)).join(fields)}
  ],
  codeSets: [
{("," + chr(10)).join(code_sets)}
  ],
  constraints: [
{("," + chr(10)).join(pk)}
  ]
}})

export const Mapping = Norn.mapping({{
  id: "{mid}", title: "distributor-1 {key} to canon and distributor types", version: "0.1.0",
  source: {{schemaId: "{sid}", version: "0.1.0", sha256: "{PLACEHOLDER}"}},
  sourceContract: {{kind: "generic"}},
  interfaces: [{", ".join(ifaces)}],
  maps: [
{("," + chr(10)).join(maps)}
  ]
}})
"""
    n_fields = sum(len(d["fields"]) for d in spec["datasets"].values())
    return src, {"track": "new", "maps": len(maps), "fields": n_fields, "coverage": cov, "ownInterfaceTypes": own}


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    report = {}
    for system in PACK_REBIND:
        text, stats = rebind_pack(system)
        (out / f"pack_{system}.ts").write_text(text)
        report[f"pack_{system}"] = stats
    for key, spec in SPEC.items():
        text, stats = gen_module(key, spec)
        (out / f"{key}.ts").write_text(text)
        report[key] = stats
    (out / "coverage.json").write_text(json.dumps(report, indent=1))
    print(json.dumps({k: {kk: v[kk] for kk in ("track", "maps", "fields")} for k, v in report.items()}))


if __name__ == "__main__":
    main()
