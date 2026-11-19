#!/usr/bin/env python3
"""Seed ERPNext, Odoo and Saleor with each system's share of world.json.

Talks to the systems directly (not through the hostile proxy): seeding is
setup, not the test. Every step is tolerant: a failure is recorded in
seed-report.json and the run goes on, because "what breaks" is the output.

Usage: seed.py WORLD_JSON OUT_DIR [erpnext] [odoo] [saleor]
"""
from __future__ import annotations

import json
import sys
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extract"))
from d1http import STATS, Client  # noqa: E402

REPORT: dict = {"steps": {}, "errors": []}
MAX_ERRORS_PER_STEP = 5


def step(name: str):
    def wrap(fn):
        def run(*a, **k):
            t = time.time()
            ok = err = 0
            try:
                for _ in fn(*a, **k) or []:
                    ok += 1
            except Exception as e:  # the step as a whole broke
                err += 1
                REPORT["errors"].append({"step": name, "error": str(e)[:600], "trace": traceback.format_exc()[-800:]})
            REPORT["steps"][name] = {**REPORT["steps"].get(name, {}), "ok": ok + REPORT["steps"].get(name, {}).get("ok", 0), "fatal": err, "seconds": round(time.time() - t, 1)}
        return run
    return wrap


def tolerant(name: str, fn, *a):
    """One record: count failures, keep the first few messages."""
    try:
        return fn(*a)
    except Exception as e:
        s = REPORT["steps"].setdefault(name, {})
        s["failed"] = s.get("failed", 0) + 1
        if s["failed"] <= MAX_ERRORS_PER_STEP:
            REPORT["errors"].append({"step": name, "error": str(e)[:500]})
        return None


# ------------------------------------------------------------------ ERPNext
WH_ERP = {"WH-CAI": "WH-CAI - D1", "WH-ALX": "WH-ALX - D1"}


def erp_doc(c: Client, doctype: str, doc: dict, submit: bool = False):
    d = c.post(f"/api/resource/{doctype}", doc)["data"]
    if submit:
        d = c.post("/api/method/frappe.client.submit", {"doc": d})["message"]
    return d


def erp_method(c: Client, method: str, args: dict):
    return c.post(f"/api/method/{method}", args)["message"]


@step("erpnext.masters")
def erp_masters(c: Client, w: dict):
    for t in ("Cairo", "Alexandria", "Giza", "Delta", "Upper Egypt"):
        if tolerant("erpnext.territory", erp_doc, c, "Territory", {"territory_name": t, "parent_territory": "All Territories"}):
            yield t
    tolerant("erpnext.customer_group", erp_doc, c, "Customer Group", {"customer_group_name": "Wholesale", "parent_customer_group": "All Customer Groups"})
    for u in ("Carton", "Box", "Kg", "Litre"):
        tolerant("erpnext.uom", erp_doc, c, "UOM", {"uom_name": u})
    for g in sorted({i["group"] for i in w["items"]}):
        tolerant("erpnext.item_group", erp_doc, c, "Item Group", {"item_group_name": g, "parent_item_group": "All Item Groups"})
    for code in WH_ERP:
        tolerant("erpnext.warehouse", erp_doc, c, "Warehouse", {"warehouse_name": code, "company": "Distributor One"})
    tolerant("erpnext.fy", erp_doc, c, "Fiscal Year", {"year": "2026", "year_start_date": "2026-01-01", "year_end_date": "2026-12-31"})
    tolerant("erpnext.stock_settings", c.put, "/api/resource/Stock Settings/Stock Settings", {"allow_negative_stock": 1})
    for it in w["items"]:
        if tolerant("erpnext.item", erp_doc, c, "Item", {"item_code": it["erpnext_code"], "item_name": it["name_en"], "item_group": it["group"], "stock_uom": it["uom"],
                                                          "is_stock_item": 1, "standard_rate": str(it["price"]), "valuation_rate": str(it["cost"])}):
            yield it["id"]
    for cu in w["customers"]:
        if "erpnext" in cu["in"] and tolerant("erpnext.customer", erp_doc, c, "Customer", {
                "customer_name": cu["name_en"], "customer_type": "Company", "customer_group": "Wholesale", "territory": cu["region"], "tax_id": cu["tax_id"],
                "credit_limits": [{"company": "Distributor One", "credit_limit": str(cu["credit_limit"])}]}):
            yield cu["id"]


@step("erpnext.stock")
def erp_stock(c: Client, w: dict):
    items = {i["id"]: i for i in w["items"]}
    rows = [{"item_code": items[s["item"]]["erpnext_code"], "warehouse": WH_ERP[s["wh"]], "qty": s["qty"], "valuation_rate": str(items[s["item"]]["cost"])}
            for s in w["opening_stock"] if s["wh"] in WH_ERP]
    for k in range(0, len(rows), 100):
        if tolerant("erpnext.opening", erp_doc, c, "Stock Reconciliation", {"company": "Distributor One", "purpose": "Opening Stock", "posting_date": "2025-01-01",
                                                                           "set_posting_time": 1, "expense_account": "Temporary Opening - D1", "items": rows[k:k + 100]}, True):
            yield k
    for r in w["receipts"]:
        if r["wh"] in WH_ERP and tolerant("erpnext.receipt", erp_doc, c, "Stock Entry", {
                "stock_entry_type": "Material Receipt", "company": "Distributor One", "posting_date": r["at"][:10], "set_posting_time": 1,
                "items": [{"item_code": items[r["item"]]["erpnext_code"], "t_warehouse": WH_ERP[r["wh"]], "qty": r["qty"], "basic_rate": str(items[r["item"]]["cost"])}]}, True):
            yield r["id"]


@step("erpnext.sales")
def erp_sales(c: Client, w: dict, crosswalk: dict):
    items = {i["id"]: i for i in w["items"]}
    cust_name = {cu["id"]: cu["name_en"] for cu in w["customers"]}
    delivered = {d["order"]: d for d in w["deliveries"]}
    invoiced = {i["order"]: i for i in w["invoices"]}
    paid = {p["invoice"]: p for p in w["payments"] if p["invoice"]}
    for o in [o for o in w["orders"] if o["system"] == "erpnext" and o["warehouse"] in WH_ERP]:
        def one(o=o):
            so = erp_doc(c, "Sales Order", {"customer": cust_name[o["customer"]], "company": "Distributor One", "transaction_date": o["at"][:10], "delivery_date": o["at"][:10],
                                            "set_warehouse": WH_ERP[o["warehouse"]], "currency": "EGP", "selling_price_list": "Standard Selling",
                                            "items": [{"item_code": items[l["item"]]["erpnext_code"], "qty": l["qty"], "rate": str(l["rate"]), "delivery_date": o["at"][:10]} for l in o["lines"]]}, True)
            crosswalk["erpnext_so"][o["id"]] = so["name"]
            d = delivered.get(o["id"])
            if not d:
                return so
            dn = erp_method(c, "erpnext.selling.doctype.sales_order.sales_order.make_delivery_note", {"source_name": so["name"]})
            dn.update({"posting_date": d["at"][:10], "posting_time": d["at"][11:19], "set_posting_time": 1})
            dn = c.post("/api/method/frappe.client.submit", {"doc": c.post("/api/method/frappe.client.insert", {"doc": dn})["message"]})["message"]
            crosswalk["erpnext_dn"][d["id"]] = dn["name"]
            inv = invoiced.get(o["id"])
            if not inv:
                return so
            si = erp_method(c, "erpnext.stock.doctype.delivery_note.delivery_note.make_sales_invoice", {"source_name": dn["name"]})
            si.update({"posting_date": inv["at"], "set_posting_time": 1, "due_date": inv["at"]})
            si = c.post("/api/method/frappe.client.submit", {"doc": c.post("/api/method/frappe.client.insert", {"doc": si})["message"]})["message"]
            crosswalk["erpnext_si"][inv["id"]] = si["name"]
            p = paid.get(inv["id"])
            if p:
                pe = erp_method(c, "erpnext.accounts.doctype.payment_entry.payment_entry.get_payment_entry", {"dt": "Sales Invoice", "dn": si["name"]})
                pe.update({"posting_date": p["at"], "reference_no": p["id"], "reference_date": p["at"], "paid_amount": str(p["amount"]), "received_amount": str(p["amount"])})
                for ref in pe.get("references", []):
                    ref["allocated_amount"] = str(min(float(p["amount"]), float(ref.get("outstanding_amount") or p["amount"])))
                pe = c.post("/api/method/frappe.client.submit", {"doc": c.post("/api/method/frappe.client.insert", {"doc": pe})["message"]})["message"]
                crosswalk["erpnext_pe"][p["id"]] = pe["name"]
            return so
        if tolerant("erpnext.order_chain", one):
            yield o["id"]


# --------------------------------------------------------------------- Odoo
class Odoo:
    def __init__(self, base: str):
        self.c = Client(base, "odoo")
        self.uid = self.rpc("common", "login", ["d1", "admin", "admin"])

    def rpc(self, service: str, method: str, args: list):
        r = self.c.post("/jsonrpc", {"jsonrpc": "2.0", "method": "call", "params": {"service": service, "method": method, "args": args}, "id": 1})
        if r.get("error"):
            raise RuntimeError(json.dumps(r["error"])[:500])
        return r.get("result")  # Odoo omits `result` for a method that returns None

    def x(self, model: str, method: str, args: list, kw: dict | None = None):
        return self.rpc("object", "execute_kw", ["d1", self.uid, "admin", model, method, args, kw or {}])


WH_ODOO = {"WH-GIZ": "GIZ", "WH-DLT": "DLT"}


@step("odoo.masters")
def odoo_masters(o: Odoo, w: dict, xw: dict):
    egp = o.x("res.currency", "search", [[["name", "=", "EGP"], ["active", "in", [True, False]]]])
    o.x("res.currency", "write", [egp, {"active": True}])
    tolerant("odoo.company_currency", o.x, "res.company", "write", [[1], {"currency_id": egp[0]}])
    for code, short in WH_ODOO.items():
        wid = tolerant("odoo.warehouse", o.x, "stock.warehouse", "create", [{"name": code, "code": short}])
        if wid:
            xw["odoo_wh"][code] = o.x("stock.warehouse", "read", [[wid], ["lot_stock_id"]])[0]["lot_stock_id"][0]
            yield code
    for it in w["items"]:
        pid = tolerant("odoo.product", o.x, "product.product", "create", [{"name": it["name_ar"], "default_code": it["odoo_code"], "list_price": float(it["price"]),
                                                                            "standard_price": float(it["cost"]), "detailed_type": "product"}])
        if pid:
            xw["odoo_product"][it["id"]] = pid
            yield it["id"]
    for cu in w["customers"]:
        if "odoo" not in cu["in"]:
            continue
        limit = float(cu.get("conflict_credit_limit_odoo") or cu["credit_limit"])
        pid = tolerant("odoo.partner", o.x, "res.partner", "create", [{"name": cu["name_ar"], "city": cu["region"], "vat": cu["tax_id"], "email": cu["email"],
                                                                        "customer_rank": 1, "credit_limit": limit, "lang": "ar_001"}])
        if pid:
            xw["odoo_partner"][cu["id"]] = pid
            yield cu["id"]


@step("odoo.stock")
def odoo_stock(o: Odoo, w: dict, xw: dict):
    for s in w["opening_stock"]:
        loc, pid = xw["odoo_wh"].get(s["wh"]), xw["odoo_product"].get(s["item"])
        if loc and pid:
            q = tolerant("odoo.quant", o.x, "stock.quant", "create", [{"product_id": pid, "location_id": loc, "inventory_quantity": s["qty"]}], {"context": {"inventory_mode": True}})
            if q and tolerant("odoo.quant_apply", o.x, "stock.quant", "action_apply_inventory", [[q]]) is not False:
                yield s["item"]


@step("odoo.sales")
def odoo_sales(o: Odoo, w: dict, xw: dict):
    delivered = {d["order"] for d in w["deliveries"]}
    invoiced = {i["order"]: i for i in w["invoices"]}
    paid = {p["invoice"]: p for p in w["payments"] if p["invoice"]}
    whs = {code: o.x("stock.warehouse", "search", [[["code", "=", short]]]) for code, short in WH_ODOO.items()}
    for so in [x for x in w["orders"] if x["system"] == "odoo" and x["customer"] in xw["odoo_partner"]]:
        def one(so=so):
            wh = whs.get(so["warehouse"]) or next(iter(whs.values()))
            oid = o.x("sale.order", "create", [{"partner_id": xw["odoo_partner"][so["customer"]], "date_order": so["at"].replace("T", " "), "warehouse_id": wh[0],
                                                 "order_line": [[0, 0, {"product_id": xw["odoo_product"][l["item"]], "product_uom_qty": l["qty"], "price_unit": float(l["rate"])}] for l in so["lines"]]}])
            o.x("sale.order", "action_confirm", [[oid]])
            xw["odoo_so"][so["id"]] = oid
            if so["id"] not in delivered:
                return oid
            for pick in o.x("stock.picking", "search", [[["sale_id", "=", oid]]]):
                for mv in o.x("stock.move", "search_read", [[["picking_id", "=", pick]]], {"fields": ["product_uom_qty"]}):
                    o.x("stock.move", "write", [[mv["id"]], {"quantity": mv["product_uom_qty"], "picked": True}])
                o.x("stock.picking", "button_validate", [[pick]])
            inv = invoiced.get(so["id"])
            if not inv:
                return oid
            wiz = o.x("sale.advance.payment.inv", "create", [{"advance_payment_method": "delivered"}], {"context": {"active_model": "sale.order", "active_ids": [oid]}})
            o.x("sale.advance.payment.inv", "create_invoices", [[wiz]], {"context": {"active_model": "sale.order", "active_ids": [oid]}})
            moves = o.x("account.move", "search", [[["invoice_origin", "=", o.x("sale.order", "read", [[oid], ["name"]])[0]["name"]], ["move_type", "=", "out_invoice"]]])
            o.x("account.move", "write", [moves, {"invoice_date": inv["at"]}])
            o.x("account.move", "action_post", [moves])
            xw["odoo_inv"][inv["id"]] = moves[0]
            p = paid.get(inv["id"])
            if p:
                reg = o.x("account.payment.register", "create", [{"amount": float(p["amount"]), "payment_date": p["at"], "communication": p["id"]}],
                          {"context": {"active_model": "account.move", "active_ids": moves}})
                o.x("account.payment.register", "action_create_payments", [[reg]], {"context": {"active_model": "account.move", "active_ids": moves}})
            return oid
        if tolerant("odoo.order_chain", one):
            yield so["id"]


# ------------------------------------------------------------------- Saleor
class Saleor:
    def __init__(self, base: str, email: str, password: str):
        self.c = Client(base, "saleor")
        tok = self.q("mutation($e:String!,$p:String!){tokenCreate(email:$e,password:$p){token errors{message}}}", {"e": email, "p": password})["tokenCreate"]
        if not tok["token"]:
            raise RuntimeError(f"saleor login: {tok['errors']}")
        self.c.headers["Authorization"] = f"Bearer {tok['token']}"

    def q(self, query: str, variables: dict | None = None):
        r = self.c.post("/graphql/", {"query": query, "variables": variables or {}})
        if r.get("errors"):
            raise RuntimeError(json.dumps(r["errors"])[:500])
        data = r["data"]
        for v in data.values():  # mutation-level errors
            if isinstance(v, dict) and v.get("errors"):
                raise RuntimeError(json.dumps(v["errors"])[:500])
        return data


SALEOR_WH = ("WH-CAI", "WH-GIZ")


@step("saleor.masters")
def saleor_masters(s: Saleor, w: dict, xw: dict, webhook_url: str):
    ch = s.q("mutation{channelCreate(input:{name:\"d1 B2B\",slug:\"d1-b2b\",currencyCode:\"EGP\",defaultCountry:EG,isActive:true}){channel{id} errors{message}}}")["channelCreate"]["channel"]["id"]
    xw["saleor_channel"] = ch
    for code in SALEOR_WH:
        wh = s.q("mutation($n:String!,$s:String!){createWarehouse(input:{name:$n,slug:$s,address:{streetAddress1:\"1 Synthetic St\",city:\"CAIRO\",country:EG,postalCode:\"11511\"}}){warehouse{id} errors{message}}}",
                 {"n": code, "s": code.lower()})["createWarehouse"]["warehouse"]["id"]
        s.q("mutation($c:ID!,$w:ID!){channelUpdate(id:$c,input:{addWarehouses:[$w]}){errors{message}}}", {"c": ch, "w": wh})
        xw["saleor_wh"][code] = wh
        yield code
    pt = s.q("mutation{productTypeCreate(input:{name:\"Goods\",kind:NORMAL,isShippingRequired:false,hasVariants:false}){productType{id} errors{message}}}")["productTypeCreate"]["productType"]["id"]
    cats = {}
    for g in sorted({i["group"] for i in w["items"]}):
        cats[g] = s.q("mutation($n:String!){categoryCreate(input:{name:$n}){category{id} errors{message}}}", {"n": g})["categoryCreate"]["category"]["id"]
    stock = {(x["item"], x["wh"]): x["qty"] for x in w["closing_stock"]}
    for it in w["items"]:
        def one(it=it):
            p = s.q("mutation($t:ID!,$c:ID!,$n:String!){productCreate(input:{productType:$t,category:$c,name:$n}){product{id} errors{message}}}",
                    {"t": pt, "c": cats[it["group"]], "n": it["name_en"]})["productCreate"]["product"]["id"]
            s.q("mutation($p:ID!,$c:ID!){productChannelListingUpdate(id:$p,input:{updateChannels:[{channelId:$c,isPublished:true,isAvailableForPurchase:true,visibleInListings:true}]}){errors{message}}}", {"p": p, "c": ch})
            stocks = [{"warehouse": xw["saleor_wh"][wh], "quantity": 100000} for wh in SALEOR_WH]  # set to closing after the orders
            v = s.q("mutation($p:ID!,$sku:String!,$st:[StockInput!]){productVariantCreate(input:{product:$p,sku:$sku,attributes:[],trackInventory:true,stocks:$st}){productVariant{id} errors{message}}}",
                    {"p": p, "sku": it.get("saleor_sku_alias_of") and it["saleor_sku"] + "-B" or it["saleor_sku"], "st": stocks})["productVariantCreate"]["productVariant"]["id"]
            s.q("mutation($v:ID!,$c:ID!,$pr:PositiveDecimal!){productVariantChannelListingUpdate(id:$v,input:[{channelId:$c,price:$pr}]){errors{message}}}", {"v": v, "c": ch, "pr": str(it["web_price"])})
            xw["saleor_variant"][it["id"]] = v
            return v
        if tolerant("saleor.product", one):
            yield it["id"]
    for cu in w["customers"]:
        if "saleor" in cu["in"]:
            u = tolerant("saleor.customer", s.q, "mutation($e:String!,$f:String!,$r:String!){customerCreate(input:{email:$e,firstName:$f,isActive:true,note:$r}){user{id} errors{message}}}",
                         {"e": cu["email"], "f": cu["name_en"], "r": f"region={cu['region']}"})
            if u:
                xw["saleor_user"][cu["id"]] = u["customerCreate"]["user"]["id"]
                yield cu["id"]
    app = tolerant("saleor.app", s.q, "mutation{appCreate(input:{name:\"d1-norn\",permissions:[MANAGE_ORDERS]}){app{id} authToken errors{message}}}")
    if app:
        tolerant("saleor.webhook", s.q, "mutation($a:ID!,$u:String!){webhookCreate(input:{name:\"d1\",app:$a,targetUrl:$u,isActive:true,asyncEvents:[ORDER_CREATED,ORDER_UPDATED,ORDER_FULLY_PAID,ORDER_FULFILLED],"
                 "query:\"subscription { event { ... on OrderCreated { order { id number status updatedAt } } ... on OrderUpdated { order { id number status updatedAt } } ... on OrderFullyPaid { order { id number status updatedAt } } ... on OrderFulfilled { order { id number status updatedAt } } } }\"}){webhook{id} errors{message}}}",
                 {"a": app["appCreate"]["app"]["id"], "u": webhook_url})


@step("saleor.sales")
def saleor_sales(s: Saleor, w: dict, xw: dict):
    delivered = {d["order"] for d in w["deliveries"]}
    paid_orders = {i["order"] for i in w["invoices"] if any(p["invoice"] == i["id"] for p in w["payments"])}
    for so in [x for x in w["orders"] if x["system"] == "saleor" and x["customer"] in xw["saleor_user"]]:
        def one(so=so):
            lines = [{"variantId": xw["saleor_variant"][l["item"]], "quantity": l["qty"]} for l in so["lines"] if l["item"] in xw["saleor_variant"]]
            d = s.q("mutation($u:ID!,$c:ID!,$l:[OrderLineCreateInput!]){draftOrderCreate(input:{user:$u,channelId:$c,lines:$l,billingAddress:{firstName:\"Buyer\",streetAddress1:\"1 Synthetic St\",city:\"CAIRO\",country:EG,postalCode:\"11511\"}}){order{id} errors{message}}}",
                    {"u": xw["saleor_user"][so["customer"]], "c": xw["saleor_channel"], "l": lines})["draftOrderCreate"]["order"]["id"]
            o = s.q("mutation($id:ID!){draftOrderComplete(id:$id){order{id number lines{id quantity}} errors{message}}}", {"id": d})["draftOrderComplete"]["order"]
            xw["saleor_order"][so["id"]] = o["number"]
            if so["id"] in paid_orders:
                s.q("mutation($id:ID!){orderMarkAsPaid(id:$id){errors{message}}}", {"id": o["id"]})
            if so["id"] in delivered:
                wh = xw["saleor_wh"].get(so["warehouse"]) or xw["saleor_wh"]["WH-CAI"]
                s.q("mutation($id:ID!,$l:[OrderFulfillLineInput!]!){orderFulfill(order:$id,input:{lines:$l,allowStockToBeExceeded:true}){errors{message}}}",
                    {"id": o["id"], "l": [{"orderLineId": ln["id"], "stocks": [{"quantity": ln["quantity"], "warehouse": wh}]} for ln in o["lines"]]})
            return o
        if tolerant("saleor.order_chain", one):
            yield so["id"]
    # the shop's stock: the world's closing balance per variant and warehouse
    stock = {(x["item"], x["wh"]): x["qty"] for x in w["closing_stock"]}
    for iid, v in xw["saleor_variant"].items():
        tolerant("saleor.stock_set", s.q, "mutation($v:ID!,$st:[StockInput!]!){productVariantStocksUpdate(variantId:$v,stocks:$st){errors{message}}}",
                 {"v": v, "st": [{"warehouse": xw["saleor_wh"][wh], "quantity": max(0, stock.get((iid, wh), 0))} for wh in SALEOR_WH]})


def saleor_lookup(s: "Saleor", w: dict, xw: dict) -> None:
    """Re-seeding orders only: find the channel, warehouses, variants and users a first pass created."""
    xw["saleor_channel"] = s.q("{channel(slug:\"d1-b2b\"){id}}")["channel"]["id"]
    s.q("mutation($c:ID!){channelUpdate(id:$c,input:{isActive:true}){errors{message}}}", {"c": xw["saleor_channel"]})
    for e in s.q("{warehouses(first:20){edges{node{id name}}}}")["warehouses"]["edges"]:
        xw["saleor_wh"][e["node"]["name"]] = e["node"]["id"]
    sku = {i["saleor_sku"]: i["id"] for i in w["items"]}
    after = None
    while True:
        r = s.q("query($a:String){productVariants(first:100,after:$a){pageInfo{hasNextPage endCursor} edges{node{id sku}}}}", {"a": after})["productVariants"]
        for e in r["edges"]:
            if e["node"]["sku"] in sku:
                xw["saleor_variant"][sku[e["node"]["sku"]]] = e["node"]["id"]
        if not r["pageInfo"]["hasNextPage"]:
            break
        after = r["pageInfo"]["endCursor"]
    email = {c["email"]: c["id"] for c in w["customers"]}
    r = s.q("{customers(first:100){edges{node{id email}}}}")["customers"]["edges"]
    for e in r:
        if e["node"]["email"] in email:
            xw["saleor_user"][email[e["node"]["email"]]] = e["node"]["id"]


def main() -> None:
    world = json.loads(Path(sys.argv[1]).read_text())
    out = Path(sys.argv[2])
    which = set(sys.argv[3:]) or {"erpnext", "odoo", "saleor"}
    prior = out / "native-ids.json"
    xw = {k: {} for k in ("erpnext_so", "erpnext_dn", "erpnext_si", "erpnext_pe", "odoo_wh", "odoo_product", "odoo_partner", "odoo_so", "odoo_inv", "saleor_wh", "saleor_variant", "saleor_user", "saleor_order")}
    if prior.exists():  # keep the ids of systems seeded by an earlier pass
        for k, v in json.loads(prior.read_text()).items():
            if v and not xw.get(k):
                xw[k] = v
    t0 = time.time()
    if "erpnext" in which:
        c = Client("http://127.0.0.1:8000", "erpnext-seed", {"Host": "d1.localhost"})
        c.post("/api/method/login", {"usr": "Administrator", "pwd": "d1-admin"})
        erp_masters(c, world)
        erp_stock(c, world)
        erp_sales(c, world, xw)
    if "odoo" in which:
        try:
            o = Odoo("http://127.0.0.1:8069")
            odoo_masters(o, world, xw)
            odoo_stock(o, world, xw)
            odoo_sales(o, world, xw)
        except Exception as e:
            REPORT["errors"].append({"step": "odoo.login", "error": str(e)[:500]})
    if "saleor" in which or "saleor-orders" in which:
        try:
            s = Saleor("http://127.0.0.1:8001", "admin@d1.example", "d1-admin")
            if "saleor-orders" in which:
                saleor_lookup(s, world, xw)
            else:
                saleor_masters(s, world, xw, "http://host.docker.internal:18900/")
            saleor_sales(s, world, xw)
        except Exception as e:
            REPORT["errors"].append({"step": "saleor.login", "error": str(e)[:500]})
    REPORT["seconds"] = round(time.time() - t0, 1)
    REPORT["http"] = STATS
    prev = out / "seed-report.json"
    if prev.exists():  # one report across passes: earlier steps stay, a re-run step replaces its entry
        old = json.loads(prev.read_text())
        REPORT["steps"] = {**old.get("steps", {}), **REPORT["steps"]}
        REPORT["passes"] = old.get("passes", [{"seconds": old.get("seconds"), "errors": old.get("errors", [])[:20]}]) + [{"which": sorted(which), "seconds": REPORT.get("seconds")}]
    (out / "seed-report.json").write_text(json.dumps(REPORT, indent=1))
    (out / "native-ids.json").write_text(json.dumps(xw, indent=1))
    print(json.dumps({k: v for k, v in REPORT["steps"].items()}))


if __name__ == "__main__":
    main()
