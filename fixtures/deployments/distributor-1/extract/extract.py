#!/usr/bin/env python3
"""Pull every distributor-1 system through the hostile proxy and write Norn
recorded exports (the body of POST /v1/observations:load).

Two modes, so the report can say what chaos does to Norn rather than to us:
  careful  dedupe by key, reconcile counts and refetch, zone-less times get the
           site zone (+02:00), drifted field names are mapped back
  naive    take what the wire returned

The crosswalk (native key -> canonical ref) is what a deployment team authors
by hand from its master-data spreadsheet; here it is derived from world.json,
minus world["unmapped_customers"], which are left out on purpose.

Usage: extract.py WORLD_JSON FILES_DIR OUT_DIR careful|naive [erpnext odoo saleor legacy files]
"""
from __future__ import annotations

import csv
import io
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from d1http import STATS, Client, bump  # noqa: E402
import files as filesrc  # noqa: E402

SITE_TZ = "+02:00"
UCUM = {"Box": "{each}", "Carton": "{each}", "Nos": "{each}", "Units": "{each}", "Unit": "{each}", "Kg": "kg", "kg": "kg", "Litre": "L"}  # L has no canon unit
SELF = "d1/party/self"
ANOM: dict[str, dict[str, int]] = {}


def anomaly(system: str, kind: str, n: int = 1) -> None:
    ANOM.setdefault(system, {}).setdefault(kind, 0)
    ANOM[system][kind] += n


def ts(text: str | None, careful: bool) -> str | None:
    """ERPNext/Odoo/WMS times have no zone. Careful: the site zone; naive: as is."""
    if not text:
        return text
    t = text.replace(" ", "T")
    if len(t) == 10:
        t += "T00:00:00"
    t = t.split(".")[0]
    if careful and not (t.endswith("Z") or "+" in t[10:] or t[10:].count("-") > 0):
        return t + SITE_TZ
    return t if careful else text


class Crosswalk:
    def __init__(self, world: dict):
        skip = set(world["unmapped_customers"])
        self.party_by_en = {c["name_en"]: f"d1/party/{c['id']}" for c in world["customers"] if c["id"] not in skip}
        self.party_by_ar = {c["name_ar"]: f"d1/party/{c['id']}" for c in world["customers"] if c["id"] not in skip}
        self.party_by_email = {c["email"]: f"d1/party/{c['id']}" for c in world["customers"] if c["id"] not in skip}
        self.party_by_legacy = {c["id"].replace("C", "K"): f"d1/party/{c['id']}" for c in world["customers"] if c["id"] not in skip}
        self.region = {f"d1/party/{c['id']}": c["region"] for c in world["customers"]}
        self.item = {}
        for i in world["items"]:
            ref = f"d1/item/{i['id']}"
            for k in ("erpnext_code", "odoo_code", "saleor_sku", "legacy_code"):
                self.item[i[k]] = ref
        self.invoice_by_world = {}

    def holding(self, wh: str, item_ref: str | None) -> str | None:
        return f"d1/holding/{wh}/{item_ref.rsplit('/', 1)[1]}" if item_ref else None


CAREFUL = {"on": True}


def norm(v):
    """Careful mode sends exact decimal text, as ERPNext's own exports do; naive sends the wire's floats."""
    if CAREFUL["on"] and isinstance(v, float):
        return f"{v:.6f}".rstrip("0").rstrip(".") or "0"
    if CAREFUL["on"] and isinstance(v, int) and not isinstance(v, bool):
        return str(v)
    return v


def export(source: str, mapping: str, snapshot: list, incremental: list | None = None, profile: str = "d1-recorded/1") -> dict:
    """`source` is the source schema's system; the mapping is named, so one system can carry several."""
    snapshot = [(ds, {k: norm(v) for k, v in r.items()}) for ds, r in snapshot]
    return {"profile": profile, "source": source, "mapping": mapping, "connectorInstance": "distributor-1",
            "snapshot": [{"dataset": ds, "record": r} for ds, r in snapshot], **({"incremental": incremental} if incremental else {})}


class Lenient(dict):
    """A naive extractor's row: a field the wire dropped reads as null instead of crashing the pull,
    so the damage reaches Norn and the report shows what Norn does with it."""
    def __missing__(self, key):
        anomaly("wire", "missing_field_read")
        return None


def dedupe(system: str, rows: list[dict], key: str, careful: bool) -> list[dict]:
    seen, out = set(), []
    rows = rows if careful else [Lenient(r) for r in rows]
    for r in rows:
        k = r.get(key)
        if k in seen:
            anomaly(system, "duplicate_rows_seen")
            if careful:
                continue
        seen.add(k)
        out.append(r)
    return out


# ------------------------------------------------------------------ ERPNext
def erp_list(c: Client, doctype: str, fields: list[str], parent: str | None = None, filters: list | None = None, careful: bool = True, page: int = 100) -> list[dict]:
    rows, start = [], 0
    q = lambda s: (f"/api/method/frappe.client.get_list?doctype={doctype.replace(' ', '%20')}&fields={json.dumps(fields)}&limit_start={s}&limit_page_length={page}&order_by=name%20asc"
                   + (f"&parent={parent.replace(' ', '%20')}" if parent else "") + (f"&filters={json.dumps(filters)}" if filters else ""))
    while True:
        batch = c.get(q(start))["message"]
        rows += batch
        if len(batch) < page:
            break
        start += page
    rows = dedupe("erpnext", rows, "name", careful)
    # get_count takes no `parent` (child doctypes count as is)
    count = c.get(f"/api/method/frappe.client.get_count?doctype={doctype.replace(' ', '%20')}" + (f"&filters={json.dumps(filters)}" if filters else ""))["message"]
    if len(rows) != count:
        anomaly("erpnext", "count_mismatch_rows", abs(count - len(rows)))
        if careful:  # refetch in one page; skips/repeats are per request
            rows = dedupe("erpnext", c.get(q(0).replace(f"limit_page_length={page}", f"limit_page_length={count + 10}"))["message"], "name", True)
    if careful:  # schema drift: the proxy renames actual_qty mid-sync
        for r in rows:
            if "actual_quantity" in r:
                r["actual_qty"] = r.pop("actual_quantity")
                anomaly("erpnext", "drift_repaired")
    return rows


def pull_erpnext(base: str, xw: Crosswalk, careful: bool) -> dict:
    c = Client(base, "erpnext", {"Host": "d1.localhost"})
    c.post("/api/method/login", {"usr": "Administrator", "pwd": "d1-admin"})
    customers = erp_list(c, "Customer", ["name", "customer_name", "territory", "tax_id", "customer_group", "default_currency", "modified"], careful=careful)
    limits = {r["parent"]: r["credit_limit"] for r in erp_list(c, "Customer Credit Limit", ["name", "parent", "credit_limit"], parent="Customer", careful=careful)}
    items = erp_list(c, "Item", ["name", "item_name", "item_group", "stock_uom", "standard_rate", "valuation_rate", "modified"], careful=careful)
    uom = {i["name"]: i.get("stock_uom") for i in items}
    bins = erp_list(c, "Bin", ["name", "item_code", "warehouse", "actual_qty", "reserved_qty", "stock_value", "valuation_rate", "stock_uom", "modified"], careful=careful)
    so_items = erp_list(c, "Sales Order Item", ["name", "parent", "item_code", "qty", "stock_uom", "delivery_date", "warehouse"], parent="Sales Order", careful=careful)
    so = {r["name"]: r for r in erp_list(c, "Sales Order", ["name", "customer", "docstatus"], careful=careful)}
    dn_items = erp_list(c, "Delivery Note Item", ["name", "parent", "item_code", "qty", "stock_uom", "warehouse", "so_detail", "against_sales_order"], parent="Delivery Note", careful=careful)
    dn = {r["name"]: r for r in erp_list(c, "Delivery Note", ["name", "customer", "posting_date", "posting_time", "docstatus"], careful=careful)}
    si = erp_list(c, "Sales Invoice", ["name", "customer", "posting_date", "posting_time", "currency", "grand_total", "territory", "docstatus"], careful=careful)
    si_items = erp_list(c, "Sales Invoice Item", ["name", "parent", "item_code", "qty", "rate", "amount"], parent="Sales Invoice", careful=careful)
    gl = erp_list(c, "GL Entry", ["name", "account", "debit", "credit", "account_currency", "voucher_type", "voucher_no", "posting_date", "is_cancelled"], careful=careful)
    accounts = {a["name"]: a.get("account_type") or a.get("root_type") for a in erp_list(c, "Account", ["name", "account_type", "root_type"], careful=careful)}
    pe = erp_list(c, "Payment Entry", ["name", "party", "paid_amount", "paid_to_account_currency", "payment_type", "posting_date", "reference_no", "docstatus"], careful=careful)
    sle = erp_list(c, "Stock Ledger Entry", ["name", "item_code", "warehouse", "actual_qty", "qty_after_transaction", "posting_date", "posting_time", "voucher_no", "voucher_type", "is_cancelled"], careful=careful)

    cref = {r["name"]: xw.party_by_en.get(r["customer_name"]) for r in customers}
    wh = lambda w: (w or "").replace(" - D1", "")
    party_rows = [("Party", {"name": "self", "custom_norn_ref": SELF, "label": "Distributor One", "role": "owner"})]
    party_rows += [("Party", {"name": r["name"], "custom_norn_ref": cref[r["name"]], "label": r["customer_name"], "role": "customer"}) for r in customers]
    pack = list(party_rows)
    pack += [("Item", {"name": i["name"], "custom_norn_ref": xw.item.get(i["name"]), "code": i["name"], "label": i["item_name"], "sku": i["name"], "price": i.get("standard_rate")}) for i in items]
    for b in bins:
        iref = xw.item.get(b["item_code"])
        pack.append(("Warehouse Lot Balance", {"name": b["name"], "custom_norn_ref": xw.holding(wh(b["warehouse"]), iref), "resource_code": b["item_code"], "quantity": b.get("actual_qty"),
                                               "unit": UCUM.get(b.get("stock_uom") or uom.get(b["item_code"]), b.get("stock_uom")), "location": wh(b["warehouse"]), "item_ref": iref,
                                               "owner_ref": SELF, "custodian_ref": SELF, "stock_cost": b.get("stock_value"), "cost_currency": "EGP", "valuation_method": "fifo",
                                               "modified": ts(b.get("modified"), careful)}))
    for r in so_items:
        iref = xw.item.get(r["item_code"])
        pack.append(("Sales Order Item", {"name": r["name"], "custom_norn_ref": f"d1/orderline/erpnext/{r['name']}", "quantity": r.get("qty"), "unit": UCUM.get(r.get("stock_uom"), r.get("stock_uom")),
                                          "due_at": ts(r.get("delivery_date"), careful), "provider_ref": SELF, "receiver_ref": cref.get(so.get(r["parent"], {}).get("customer")),
                                          "holding_ref": xw.holding(wh(r.get("warehouse")), iref)}))
    first_dn_line = {}
    for r in dn_items:
        iref = xw.item.get(r["item_code"])
        head = dn.get(r["parent"], {})
        first_dn_line.setdefault(r["parent"], f"d1/delivery/erpnext/{r['name']}")
        pack.append(("Delivery Note Item", {"name": r["name"], "custom_norn_ref": f"d1/delivery/erpnext/{r['name']}", "quantity": r.get("qty"), "unit": UCUM.get(r.get("stock_uom"), r.get("stock_uom")),
                                            "occurred_at": ts(f"{head.get('posting_date')} {head.get('posting_time')}", careful), "provider_ref": SELF, "receiver_ref": cref.get(head.get("customer")),
                                            "holding_ref": xw.holding(wh(r.get("warehouse")), iref), "order_ref": f"d1/orderline/erpnext/{r['so_detail']}" if r.get("so_detail") else None,
                                            "posting_state": "posted" if str(head.get("docstatus")) == "1" else "draft"}))
    role = {"Stock": "inventory", "Cost of Goods Sold": "cogs", "Receivable": "receivable", "Income Account": "income", "Income": "income", "Bank": "cash", "Cash": "cash", "Tax": "tax"}
    for g in gl:
        pack.append(("GL Entry", {"name": g["name"], "custom_norn_ref": f"d1/posting/erpnext/{g['name']}", "account_code": g["account"], "amount": str(round(float(g.get("debit") or 0) - float(g.get("credit") or 0), 2)),
                                  "currency": g.get("account_currency"), "book": "d1/books-egp", "account_role": role.get(accounts.get(g["account"]), accounts.get(g["account"])),
                                  # canon.Posting needs a Delivery event: only Delivery Note vouchers have one
                                  "event_ref": first_dn_line.get(g["voucher_no"]) if g["voucher_type"] == "Delivery Note" else None,
                                  "posting_state": "cancelled" if str(g.get("is_cancelled")) == "1" else "posted"}))
    for p in pe:
        pack.append(("Payment Entry", {"name": p["name"], "custom_norn_ref": f"d1/settlement/erpnext/{p['name']}", "amount": p.get("paid_amount"), "currency": p.get("paid_to_account_currency"),
                                       "kind": "payment" if p.get("payment_type") == "Receive" else "refund", "payer_ref": cref.get(p.get("party")), "payee_ref": SELF}))
    docs = [("Customer", {"name": r["name"], "x_norn_ref": cref[r["name"]], "customer_name": r["customer_name"], "territory": r.get("territory"), "tax_id": r.get("tax_id"),
                          "credit_limit": limits.get(r["name"]), "customer_group": r.get("customer_group"), "default_currency": r.get("default_currency"), "modified": r.get("modified")}) for r in customers]
    for s in si:
        docs.append(("Sales Invoice", {"name": s["name"], "customer_ref": cref.get(s["customer"]), "posting_date": ts(f"{s['posting_date']} {s.get('posting_time') or '00:00:00'}", careful),
                                       "posting_time": s.get("posting_time"), "currency": s.get("currency"), "grand_total": s.get("grand_total"), "territory": s.get("territory"), "docstatus": str(s.get("docstatus"))}))
    cur = {s["name"]: s.get("currency") for s in si}
    docs += [("Sales Invoice Item", {"name": r["name"], "parent": r["parent"], "item_ref": xw.item.get(r["item_code"]), "qty": r.get("qty"), "rate": r.get("rate"), "amount": r.get("amount"), "currency": cur.get(r["parent"])}) for r in si_items]
    # stock ledger per item/warehouse from the SLE stream: opening = first balance before its move
    ledgers: dict[str, dict] = {}
    for e in sorted([e for e in sle if str(e.get("is_cancelled")) != "1"], key=lambda e: (e["posting_date"], e.get("posting_time") or "", e["name"])):
        key = f"{e['item_code']}@{wh(e['warehouse'])}"
        l = ledgers.setdefault(key, {"name": key, "item_ref": xw.item.get(e["item_code"]), "warehouse": wh(e["warehouse"]), "opening_qty": str(float(e["qty_after_transaction"]) - float(e["actual_qty"])), "moves": []})
        l["closing_qty"], l["as_of"] = str(e["qty_after_transaction"]), ts(f"{e['posting_date']} {e.get('posting_time') or '00:00:00'}", careful)
        l["moves"].append(e)
    for l in ledgers.values():
        docs.append(("Stock Ledger", {k: l[k] for k in ("name", "item_ref", "warehouse", "opening_qty", "closing_qty", "as_of")}))
        docs += [("Stock Ledger Entry", {"name": e["name"], "ledger": l["name"], "actual_qty": e["actual_qty"], "posting_datetime": ts(f"{e['posting_date']} {e.get('posting_time') or '00:00:00'}", careful),
                                         "voucher_no": e["voucher_no"], "voucher_type": e["voucher_type"]}) for e in l["moves"]]
    return {"pack_erpnext": export("erpnext", "d1.erpnext.mapping", pack), "erpnext_docs": export("erpnext", "d1.erpnext_docs.mapping", docs),
            "_native_fields": {"Customer": 70, "Item": 100, "Bin": 20}}


# --------------------------------------------------------------------- Odoo
def odoo_all(c: Client, uid: int, model: str, fields: list[str], domain: list | None = None, careful: bool = True, page: int = 80) -> list[dict]:
    def call(method, args, kw):
        r = c.post("/jsonrpc", {"jsonrpc": "2.0", "method": "call", "params": {"service": "object", "method": "execute_kw", "args": ["d1", uid, "admin", model, method, args, kw]}, "id": 1})
        if r.get("error"):
            raise RuntimeError(json.dumps(r["error"])[:400])
        return r["result"]
    rows, off = [], 0
    while True:
        batch = call("search_read", [domain or []], {"fields": fields, "offset": off, "limit": page, "order": "id asc"})
        rows += batch
        if len(batch) < page:
            break
        off += page
    rows = dedupe("odoo", rows, "id", careful)
    n = call("search_count", [domain or []], {})
    if n != len(rows):
        anomaly("odoo", "count_mismatch_rows", abs(n - len(rows)))
        if careful:
            rows = dedupe("odoo", call("search_read", [domain or []], {"fields": fields, "order": "id asc"}), "id", True)
    if careful:
        for r in rows:
            if "amount_total_signed" in r and "amount_total" not in r:
                r["amount_total"] = r.pop("amount_total_signed")
                anomaly("odoo", "drift_repaired")
    return rows


def m2o(v):
    return v[0] if isinstance(v, list) and v else None


def pull_odoo(base: str, xw: Crosswalk, careful: bool) -> dict:
    c = Client(base, "odoo")
    uid = c.post("/jsonrpc", {"jsonrpc": "2.0", "method": "call", "params": {"service": "common", "method": "login", "args": ["d1", "admin", "admin"]}, "id": 1})["result"]
    partners = odoo_all(c, uid, "res.partner", ["id", "name", "city", "vat", "credit_limit", "lang", "write_date", "customer_rank"], [["customer_rank", ">", 0]], careful)
    products = odoo_all(c, uid, "product.product", ["id", "default_code", "name", "list_price", "standard_price", "uom_id", "write_date"], careful=careful)
    code = {p["id"]: p.get("default_code") for p in products}
    whs = {w["lot_stock_id"][0]: w["name"] for w in odoo_all(c, uid, "stock.warehouse", ["id", "name", "lot_stock_id"], careful=careful)}
    quants = odoo_all(c, uid, "stock.quant", ["id", "product_id", "location_id", "quantity", "reserved_quantity", "value", "write_date"], [["location_id.usage", "=", "internal"]], careful)
    sol = odoo_all(c, uid, "sale.order.line", ["id", "order_id", "product_id", "product_uom_qty", "order_partner_id", "state", "warehouse_id"], careful=careful)
    moves = odoo_all(c, uid, "stock.move", ["id", "product_id", "quantity", "location_id", "location_dest_id", "date", "sale_line_id", "state", "partner_id"], [["state", "=", "done"]], careful)
    inv = odoo_all(c, uid, "account.move", ["id", "name", "partner_id", "invoice_date", "currency_id", "amount_total", "state", "move_type"], [["move_type", "=", "out_invoice"]], careful)
    aml = odoo_all(c, uid, "account.move.line", ["id", "move_id", "product_id", "quantity", "price_unit", "price_subtotal", "currency_id", "account_id", "balance", "display_type", "parent_state"], [["move_id.move_type", "=", "out_invoice"]], careful)
    pays = odoo_all(c, uid, "account.payment", ["id", "amount", "currency_id", "partner_id", "payment_type", "date", "state"], careful=careful)
    pref = {p["id"]: xw.party_by_ar.get(p["name"]) for p in partners}
    whname = lambda loc: whs.get(m2o(loc))
    pack = [("res.partner", {"id": "self", "x_norn_ref": SELF, "label": "Distributor One", "role": "owner"})]
    pack += [("res.partner", {"id": str(p["id"]), "x_norn_ref": pref[p["id"]], "label": p["name"], "role": "customer"}) for p in partners]
    pack += [("product.product", {"id": str(p["id"]), "x_norn_ref": xw.item.get(p.get("default_code")), "code": p.get("default_code"), "label": p["name"], "sku": p.get("default_code"), "price": p.get("list_price")}) for p in products]
    for q in quants:
        iref = xw.item.get(code.get(m2o(q["product_id"])))
        pack.append(("stock.quant", {"id": str(q["id"]), "x_norn_ref": xw.holding(whname(q["location_id"]) or "?", iref), "resource_code": code.get(m2o(q["product_id"])), "quantity": q["quantity"], "unit": "{each}",
                                     "location": whname(q["location_id"]), "item_ref": iref, "owner_ref": SELF, "custodian_ref": SELF, "stock_cost": q.get("value"), "cost_currency": "EGP",
                                     "valuation_method": "fifo", "write_date": ts(q.get("write_date"), careful)}))
    for l in sol:
        iref = xw.item.get(code.get(m2o(l["product_id"])))
        pack.append(("sale.order.line", {"id": str(l["id"]), "x_norn_ref": f"d1/orderline/odoo/{l['id']}", "quantity": l["product_uom_qty"], "unit": "{each}", "due_at": ts(l.get("create_date") or "2026-01-01", careful),
                                         "provider_ref": SELF, "receiver_ref": pref.get(m2o(l.get("order_partner_id"))), "holding_ref": xw.holding(l["warehouse_id"][1] if l.get("warehouse_id") else "?", iref)}))
    for m in moves:
        iref = xw.item.get(code.get(m2o(m["product_id"])))
        pack.append(("stock.move", {"id": str(m["id"]), "x_norn_ref": f"d1/delivery/odoo/{m['id']}", "quantity": m["quantity"], "unit": "{each}", "occurred_at": ts(m["date"], careful), "provider_ref": SELF,
                                    "receiver_ref": pref.get(m2o(m.get("partner_id"))), "holding_ref": xw.holding(whname(m["location_id"]) or "?", iref),
                                    "order_ref": f"d1/orderline/odoo/{m2o(m['sale_line_id'])}" if m.get("sale_line_id") else None, "posting_state": "posted"}))
    for l in aml:
        if l.get("display_type") in ("line_section", "line_note"):
            continue
        pack.append(("account.move.line", {"id": str(l["id"]), "x_norn_ref": f"d1/posting/odoo/{l['id']}", "account_code": l["account_id"][1] if l.get("account_id") else None, "amount": l.get("balance"),
                                           "currency": "EGP", "book": "d1/books-egp", "account_role": "income" if l.get("display_type") == "product" else "receivable" if l.get("display_type") == "payment_term" else "tax",
                                           "event_ref": None, "posting_state": l.get("parent_state")}))
    pack += [("account.payment", {"id": str(p["id"]), "x_norn_ref": f"d1/settlement/odoo/{p['id']}", "amount": p["amount"], "currency": p["currency_id"][1] if p.get("currency_id") else None,
                                  "kind": "payment" if p["payment_type"] == "inbound" else "refund", "payer_ref": pref.get(m2o(p.get("partner_id"))), "payee_ref": SELF}) for p in pays]
    docs = [("res.partner", {"id": str(p["id"]), "x_norn_ref": pref[p["id"]], "name": p["name"], "state": p.get("city"), "vat": p.get("vat") or None, "credit_limit": p.get("credit_limit"),
                             "lang": p.get("lang"), "write_date": p.get("write_date")}) for p in partners]
    for i in inv:
        r = pref.get(m2o(i["partner_id"]))
        docs.append(("account.move", {"id": str(i["id"]), "name": i["name"], "partner_ref": r, "invoice_date": ts(i.get("invoice_date"), careful), "currency": i["currency_id"][1] if i.get("currency_id") else None,
                                      "amount_total": i.get("amount_total"), "region": xw.region.get(r, "unknown"), "state": i["state"], "move_type": i["move_type"]}))
    docs += [("account.move.line", {"id": str(l["id"]), "move_id": str(m2o(l["move_id"])), "item_ref": xw.item.get(code.get(m2o(l.get("product_id")))), "quantity": l.get("quantity"),
                                    "price_unit": l.get("price_unit"), "price_subtotal": l.get("price_subtotal"), "currency": l["currency_id"][1] if l.get("currency_id") else None})
             for l in aml if l.get("display_type") == "product"]
    return {"pack_odoo": export("odoo", "d1.odoo.mapping", pack), "odoo_docs": export("odoo", "d1.odoo_docs.mapping", docs)}


# ------------------------------------------------------------------- Saleor
def saleor_all(c: Client, field: str, node: str, careful: bool) -> list[dict]:
    rows, after = [], None
    while True:
        r = c.post("/graphql/", {"query": f"query($a:String){{ {field}(first:50, after:$a) {{ pageInfo {{ hasNextPage endCursor }} edges {{ node {{ {node} }} }} }} }}", "variables": {"a": after}})
        if r.get("errors"):
            raise RuntimeError(json.dumps(r["errors"])[:400])
        conn = r["data"][field]
        rows += [e["node"] for e in conn["edges"]]
        if not conn["pageInfo"]["hasNextPage"]:
            break
        after = conn["pageInfo"]["endCursor"]
    return dedupe("saleor", rows, "id", careful)


def pull_saleor(base: str, xw: Crosswalk, careful: bool, webhooks: Path | None) -> dict:
    c = Client(base, "saleor")
    tok = c.post("/graphql/", {"query": "mutation{tokenCreate(email:\"admin@d1.example\",password:\"d1-admin\"){token}}"})["data"]["tokenCreate"]["token"]
    c.headers["Authorization"] = f"Bearer {tok}"
    users = saleor_all(c, "customers", "id email firstName note updatedAt", careful)
    variants = saleor_all(c, "productVariants", "id sku name updatedAt product { name } stocks { warehouse { name } quantity quantityAllocated } channelListings { price { amount currency } }", careful)
    orders = saleor_all(c, "orders", "id number created updatedAt status user { email } total { gross { amount currency } } lines { id productSku quantity unitPrice { gross { amount } } totalPrice { gross { amount } } } "
                        "fulfillments { id created status warehouse { name } lines { id quantity orderLine { id productSku } } } transactions { id chargedAmount { amount currency } } payments { id total { amount currency } capturedAmount { amount } }", careful)
    uref = {u["email"]: xw.party_by_email.get(u["email"]) for u in users}
    pack = [("Customer", {"id": "self", "metafield_norn_ref": SELF, "label": "Distributor One", "role": "owner"})]
    pack += [("Customer", {"id": u["id"], "metafield_norn_ref": uref[u["email"]], "label": u.get("firstName") or u["email"], "role": "customer"}) for u in users]
    for v in variants:
        iref = xw.item.get(v.get("sku"))
        if v.get("sku", "").endswith("-B"):
            anomaly("saleor", "sku_alias_variant")  # a second variant whose SKU only resembles another: not the same item
        price = (v.get("channelListings") or [{}])[0].get("price") or {}
        pack.append(("ProductVariant", {"id": v["id"], "metafield_norn_ref": iref, "code": v.get("sku"), "label": v["product"]["name"], "sku": v.get("sku"), "price": price.get("amount")}))
        for s in v.get("stocks") or []:
            pack.append(("InventoryLevel", {"id": f"{v['id']}@{s['warehouse']['name']}", "metafield_norn_ref": xw.holding(s["warehouse"]["name"], iref), "resource_code": v.get("sku"),
                                            "quantity": s["quantity"] - s["quantityAllocated"], "unit": "{each}", "location": s["warehouse"]["name"], "item_ref": iref,
                                            "owner_ref": SELF, "custodian_ref": SELF, "updated_at": v.get("updatedAt")}))
    docs = []
    for o in orders:
        r = uref.get((o.get("user") or {}).get("email"))
        for ln in o["lines"]:
            iref = xw.item.get(ln.get("productSku"))
            pack.append(("OrderLine", {"id": ln["id"], "metafield_norn_ref": f"d1/orderline/saleor/{ln['id']}", "quantity": ln["quantity"], "unit": "{each}", "due_at": o["created"],
                                       "provider_ref": SELF, "receiver_ref": r, "holding_ref": xw.holding("WH-CAI", iref)}))
        for f in o.get("fulfillments") or []:
            for fl in f["lines"]:
                iref = xw.item.get(fl["orderLine"]["productSku"])
                pack.append(("FulfillmentLine", {"id": fl["id"], "metafield_norn_ref": f"d1/delivery/saleor/{fl['id']}", "quantity": fl["quantity"], "unit": "{each}", "occurred_at": f["created"],
                                                 "provider_ref": SELF, "receiver_ref": r, "holding_ref": xw.holding((f.get("warehouse") or {}).get("name") or "WH-CAI", iref),
                                                 "order_ref": f"d1/orderline/saleor/{fl['orderLine']['id']}", "posting_state": "posted"}))
        for p in (o.get("payments") or []):
            pack.append(("Transaction", {"id": p["id"], "metafield_norn_ref": f"d1/settlement/saleor/{p['id']}", "amount": p["capturedAmount"]["amount"] or p["total"]["amount"], "currency": p["total"]["currency"],
                                         "kind": "payment", "payer_ref": r, "payee_ref": SELF}))
        docs.append(("Order", {"id": o["id"], "number": o["number"], "user_ref": r, "created": o["created"], "currency": o["total"]["gross"]["currency"], "total_gross": o["total"]["gross"]["amount"],
                               "region": xw.region.get(r, "unknown"), "status": o["status"]}))
        docs += [("OrderLine", {"id": ln["id"], "order_id": o["number"], "item_ref": xw.item.get(ln.get("productSku")), "quantity": ln["quantity"], "unit_price_gross": ln["unitPrice"]["gross"]["amount"],
                                "total_price_gross": ln["totalPrice"]["gross"]["amount"], "currency": o["total"]["gross"]["currency"]}) for ln in o["lines"]]
    out = {"pack_saleor": export("saleor", "d1.saleor.mapping", pack), "saleor_docs": export("saleor", "d1.saleor_docs.mapping", docs)}
    # webhooks: every delivery (duplicates and out-of-order included) as an incremental upsert of the order
    if webhooks and webhooks.exists():
        inc, by_number = [], {o["number"]: o for o in orders}
        for seq, line in enumerate(webhooks.read_text().splitlines(), 1):
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            o = (ev.get("order") or {})
            full = by_number.get(o.get("number"))
            if not full:
                continue
            r = uref.get((full.get("user") or {}).get("email"))
            inc.append({"sequence": seq, "operation": "upsert", "dataset": "Order", "record": {"id": full["id"], "number": full["number"], "user_ref": r, "created": full["created"],
                        "currency": full["total"]["gross"]["currency"], "total_gross": full["total"]["gross"]["amount"], "region": xw.region.get(r, "unknown"), "status": o.get("status") or full["status"]}})
        out["saleor_webhooks"] = export("saleor", "d1.saleor_docs.mapping", [], inc)
    return out


# ------------------------------------------------------------------- legacy
PG_PORT = {"port": "15433"}


def psql(query: str) -> list[dict]:
    """Through toxiproxy (127.0.0.1:15433) when naive, direct (5433) when careful, with the postgres image's psql."""
    cmd = ["docker", "run", "--rm", "--network", "host", "-e", "PGPASSWORD=wms", "postgres:16-alpine", "psql", "-h", "127.0.0.1", "-p", PG_PORT["port"], "-U", "wms", "-d", "wms",
           "-c", f"COPY ({query}) TO STDOUT WITH CSV HEADER"]
    for attempt in range(5):
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
        if r.returncode == 0:
            return list(csv.DictReader(io.StringIO(r.stdout)))
        bump("legacy.retry")
        time.sleep(1 + attempt)
    raise RuntimeError(r.stderr[-400:])


def pull_legacy(xw: Crosswalk, world: dict, careful: bool) -> dict:
    items = psql("SELECT * FROM wms.itm_mst ORDER BY itm_cd")
    bal = psql("SELECT * FROM wms.stk_bal ORDER BY itm_cd, whs_cd")
    txn = psql("SELECT * FROM wms.stk_txn ORDER BY txn_id")
    opening = {(f"d1/item/{s['item']}", s["wh"][-3:]): s["qty"] for s in world["opening_stock"]}  # from ERPNext's opening reconciliation
    num = lambda t: t.replace(",", "")
    snap = [("wms.itm_mst", {"id": i["itm_cd"], "itm_cd": i["itm_cd"], "x_norn_ref": xw.item.get(i["itm_cd"]), "itm_desc_ar": i["itm_desc_ar"], "uom": i["uom"], "std_cost_txt": i["std_cost_txt"], "actv": i["actv"]}) for i in items]
    for b in bal:
        iref = xw.item.get(b["itm_cd"])
        wh = "WH-" + b["whs_cd"]
        snap.append(("wms.stk_bal", {"id": f"{b['itm_cd']}@{b['whs_cd']}", "x_norn_ref": xw.holding(wh, iref), "itm_cd": b["itm_cd"], "item_ref": iref, "whs_cd": wh, "qty": num(b["qty_txt"]),
                                     "qty_txt": b["qty_txt"], "unit": "{each}", "upd_dt": ts(b["upd_dt"], careful), "owner_ref": SELF}))
        snap.append(("wms.ledger", {"id": f"{b['itm_cd']}@{b['whs_cd']}", "item_ref": iref, "whs_cd": wh, "opening_qty": str(opening.get((iref, b["whs_cd"]), 0)), "closing_qty": num(b["qty_txt"]), "upd_dt": ts(b["upd_dt"], careful)}))
    snap += [("wms.stk_txn", {"id": t["txn_id"], "txn_id": t["txn_id"], "ledger": f"{t['itm_cd']}@{t['whs_cd']}", "qty": num(t["qty_txt"]), "qty_txt": t["qty_txt"], "txn_dt": ts(t["txn_dt"], careful), "ref_no": t["ref_no"]}) for t in txn]
    return {"legacy": export("legacy-wms", "d1.legacy.mapping", snap)}


def pull_files(files_dir: Path, xw: Crosswalk, world: dict, careful: bool) -> dict:
    snap = filesrc.all_records(files_dir, xw, world, careful)
    return {"files": export("files", "d1.files.mapping", snap)}


def main() -> None:
    world = json.loads(Path(sys.argv[1]).read_text())
    files_dir, out, mode = Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4]
    careful = mode == "careful"
    CAREFUL["on"] = careful
    PG_PORT["port"] = "5433" if careful else "15433"
    which = set(sys.argv[5:]) or {"erpnext", "odoo", "saleor", "legacy", "files"}
    out.mkdir(parents=True, exist_ok=True)
    xw = Crosswalk(world)
    prior = out / "extract-report.json"
    report = json.loads(prior.read_text()) if prior.exists() else {"mode": mode, "systems": {}}  # passes per system merge
    # careful is the clean-network baseline (direct ports); naive goes through the hostile proxy
    port = (lambda direct, hostile: direct if careful else hostile)
    pulls = {"erpnext": lambda: pull_erpnext(f"http://127.0.0.1:{port(8000, 18000)}", xw, careful), "odoo": lambda: pull_odoo(f"http://127.0.0.1:{port(8069, 18069)}", xw, careful),
             "saleor": lambda: pull_saleor(f"http://127.0.0.1:{port(8001, 18001)}", xw, careful, out.parent / "webhooks.jsonl"), "legacy": lambda: pull_legacy(xw, world, careful),
             "files": lambda: pull_files(files_dir, xw, world, careful)}
    for system in [s for s in pulls if s in which]:
        t = time.time()
        try:
            exports = pulls[system]()
            for name, body in exports.items():
                if name.startswith("_"):
                    continue
                (out / f"{name}.json").write_text(json.dumps(body, ensure_ascii=False))
                report["systems"].setdefault(system, {})[name] = len(body["snapshot"]) + len(body.get("incremental", []))
            report["systems"][system]["seconds"] = round(time.time() - t, 1)
        except Exception as e:
            report["systems"][system] = {"error": str(e)[:800], "seconds": round(time.time() - t, 1)}
    for k, v in (("anomalies", ANOM), ("http", STATS)):
        report[k] = {**report.get(k, {}), **v}
    report["files_parsed"] = filesrc.PARSED or report.get("files_parsed", {})
    (out / "extract-report.json").write_text(json.dumps(report, indent=1, ensure_ascii=False))
    print(json.dumps(report["systems"]))


if __name__ == "__main__":
    main()
