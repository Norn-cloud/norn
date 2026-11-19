#!/usr/bin/env python3
"""Deterministic synthetic world for the distributor-1 deployment test.

One "true" distributor (customers, items, warehouses, orders, deliveries,
invoices, payments, stock movements), then each system's view of it, with the
overlaps and conflicts a real multi-system distributor has:

* the same customer in ERPNext (English), Odoo (Arabic) and Saleor (e-mail);
* the same item under three codes (ERPNext item_code, Odoo default_code,
  Saleor SKU) and a legacy WMS code;
* stock for one warehouse reported by ERPNext (on hand), Saleor (available)
  and the legacy WMS (text quantities, stale timestamp);
* money as strings, timezone-less dates, Arabic in Windows-1256;
* seeded defects: invoices whose stated total differs from their lines,
  stock that does not conserve, payments with no matching invoice.

Every name is synthetic. Nothing here describes a real company or person.
Stdlib only. Usage: world.py OUT_DIR [--scale N]
"""
from __future__ import annotations

import argparse
import json
import random
from datetime import date, datetime, timedelta
from decimal import Decimal
from pathlib import Path

SEED = 20261119

EN_FIRST = ["Nile", "Delta", "Pyramid", "Lotus", "Oasis", "Sphinx", "Falcon", "Papyrus", "Sinai", "Crescent",
            "Horizon", "Atlas", "Cedar", "Palm", "Amber", "Coral", "Saffron", "Jasmine", "Granite", "Harbor"]
EN_SECOND = ["Trading", "Supplies", "Wholesale", "Stores", "Distribution", "Retail", "Mart", "Logistics", "Foods", "Hardware"]
AR_FIRST = ["النيل", "الدلتا", "الهرم", "اللوتس", "الواحة", "أبو الهول", "الصقر", "البردي", "سيناء", "الهلال",
            "الأفق", "أطلس", "الأرز", "النخيل", "العنبر", "المرجان", "الزعفران", "الياسمين", "الجرانيت", "الميناء"]
AR_SECOND = ["للتجارة", "للتوريدات", "للجملة", "ستورز", "للتوزيع", "للتجزئة", "ماركت", "للخدمات اللوجستية", "للأغذية", "للأدوات"]
REGIONS = [("Cairo", "القاهرة"), ("Alexandria", "الإسكندرية"), ("Giza", "الجيزة"), ("Delta", "الدلتا"), ("Upper Egypt", "الصعيد")]
WAREHOUSES = [  # code, region, systems that hold stock for it
    ("WH-CAI", "Cairo", ["erpnext", "saleor", "legacy"]),
    ("WH-ALX", "Alexandria", ["erpnext", "legacy"]),
    ("WH-GIZ", "Giza", ["odoo", "saleor"]),
    ("WH-DLT", "Delta", ["odoo"]),
]
ITEM_GROUPS = [("Beverages", "مشروبات", "Box"), ("Detergents", "منظفات", "Carton"), ("Canned Food", "معلبات", "Carton"),
               ("Rice & Pasta", "أرز ومكرونة", "Kg"), ("Oils", "زيوت", "Litre"), ("Snacks", "تسالي", "Box")]
UOM_UCUM = {"Box": "{box}", "Carton": "{carton}", "Kg": "kg", "Litre": "L", "Nos": "{each}"}


def money(x: float) -> Decimal:
    return Decimal(str(round(x, 2))).quantize(Decimal("0.01"))


def build(scale: int) -> dict:
    rng = random.Random(SEED)
    n_customers, n_items, n_orders = 40 * scale, 50 * scale, 200 * scale

    customers = []
    for i in range(n_customers):
        f, s = rng.randrange(len(EN_FIRST)), rng.randrange(len(EN_SECOND))
        region = REGIONS[rng.randrange(len(REGIONS))]
        cid = f"C{i + 1:04d}"
        customers.append({
            "id": cid, "name_en": f"{EN_FIRST[f]} {EN_SECOND[s]} {i + 1}", "name_ar": f"{AR_FIRST[f]} {AR_SECOND[s]} {i + 1}",
            "region": region[0], "region_ar": region[1], "email": f"buyer{i + 1}@distributor-1.example",
            "tax_id": f"{rng.randrange(100_000_000, 999_999_999)}", "credit_limit": money(rng.choice([5e4, 1e5, 2.5e5])),
            # which systems know this customer: ERPNext is the books, Odoo the Giza/Delta branch, Saleor the web shop
            "in": sorted({"erpnext"} | ({"odoo"} if rng.random() < 0.5 else set()) | ({"saleor"} if rng.random() < 0.4 else set())),
        })
    # conflicts: a customer whose credit limit differs per system, a customer missing from the crosswalk
    for c in customers[: max(3, n_customers // 20)]:
        c["conflict_credit_limit_odoo"] = money(float(c["credit_limit"]) * 1.5)
    unmapped_customers = {c["id"] for c in customers[-max(2, n_customers // 30):]}

    items = []
    for i in range(n_items):
        g = ITEM_GROUPS[rng.randrange(len(ITEM_GROUPS))]
        iid = f"I{i + 1:04d}"
        price = money(rng.uniform(20, 900))
        items.append({
            "id": iid, "erpnext_code": f"ITM-{i + 1:05d}", "odoo_code": f"P{i + 1:04d}", "saleor_sku": f"SKU-{(i + 1) * 7:06d}",
            "legacy_code": f"{g[0][:3].upper()}{i + 1:04d}", "name_en": f"{g[0]} item {i + 1}", "name_ar": f"{g[1]} صنف {i + 1}",
            "group": g[0], "uom": g[2], "price": price, "cost": money(float(price) * rng.uniform(0.55, 0.8)),
            # Saleor publishes a different (web) price for a third of items: a price conflict, not an identity one
            "web_price": money(float(price) * 1.07) if rng.random() < 0.33 else price,
        })
    # two Saleor variants share one SKU-like label: duplicate SKU must not merge identities
    if len(items) > 4:
        items[3]["saleor_sku_alias_of"] = items[2]["saleor_sku"]

    start = date(2025, 1, 1)
    stock = {}  # (item, wh) -> opening qty
    for it in items:
        for wh, _, _ in WAREHOUSES:
            if rng.random() < 0.7:
                stock[(it["id"], wh)] = rng.randrange(50, 2000)

    orders, deliveries, invoices, payments, moves = [], [], [], [], []
    for n in range(n_orders):
        c = customers[rng.randrange(n_customers)]
        system = rng.choice([s for s in c["in"]])
        wh_choices = [w for w, _, sys_ in WAREHOUSES if system in sys_ or (system == "erpnext" and w in ("WH-CAI", "WH-ALX"))]
        wh = rng.choice(wh_choices or ["WH-CAI"])
        when = datetime.combine(start + timedelta(days=rng.randrange(0, 680)), datetime.min.time()) + timedelta(minutes=rng.randrange(8 * 60, 18 * 60))
        lines = []
        for ln in range(rng.randrange(1, 5)):
            it = items[rng.randrange(n_items)]
            qty = rng.randrange(1, 40)
            lines.append({"line": ln + 1, "item": it["id"], "qty": qty, "rate": it["price"], "amount": money(float(it["price"]) * qty)})
        oid = f"SO-{n + 1:06d}"
        total = sum((l["amount"] for l in lines), Decimal("0"))
        currency = "USD" if rng.random() < 0.05 else "EGP"
        orders.append({"id": oid, "system": system, "customer": c["id"], "warehouse": wh, "at": when.isoformat(), "currency": currency,
                       "lines": lines, "total": total, "region": c["region"]})
        if rng.random() < 0.8:  # delivered
            dat = when + timedelta(days=rng.randrange(0, 6))
            deliveries.append({"id": f"DN-{n + 1:06d}", "order": oid, "system": system, "at": dat.isoformat(), "warehouse": wh,
                               "lines": [{"item": l["item"], "qty": l["qty"]} for l in lines]})
            for l in lines:
                moves.append({"item": l["item"], "wh": wh, "qty": -l["qty"], "at": dat.isoformat(), "ref": f"DN-{n + 1:06d}"})
            inv_total = total
            defect = None
            if rng.random() < 0.03:  # seeded: the stated total disagrees with the lines (a dropped decimal / misread digit)
                inv_total = total * 10 if rng.random() < 0.5 else total - Decimal("1.00")
                defect = "total_mismatch"
            iid = f"SINV-{n + 1:06d}"
            invoices.append({"id": iid, "order": oid, "system": system, "customer": c["id"], "at": dat.date().isoformat(), "currency": currency,
                             "lines": lines, "stated_total": inv_total, "defect": defect, "region": c["region"]})
            if rng.random() < 0.75:
                paid = inv_total if rng.random() < 0.9 else money(float(inv_total) * rng.uniform(0.3, 0.9))
                payments.append({"id": f"PAY-{n + 1:06d}", "invoice": iid, "customer": c["id"], "amount": paid, "currency": currency,
                                 "at": (dat + timedelta(days=rng.randrange(1, 45))).date().isoformat(),
                                 "bank": rng.choice(["BANK-A", "BANK-B"]), "ref_style": rng.choice(["invoice", "invoice", "free_text", "none"])})
    # unmatched bank receipts (no invoice): settlement matching must flag them
    for k in range(max(3, n_orders // 50)):
        c = customers[rng.randrange(n_customers)]
        payments.append({"id": f"PAY-X{k + 1:04d}", "invoice": None, "customer": c["id"], "amount": money(rng.uniform(100, 5000)),
                         "currency": "EGP", "at": (start + timedelta(days=rng.randrange(0, 680))).isoformat(), "bank": "BANK-A", "ref_style": "none"})
    # receipts into stock (purchase receipts / ASNs)
    receipts = []
    for k in range(n_orders // 4):
        it = items[rng.randrange(n_items)]
        wh = rng.choice([w for w, _, _ in WAREHOUSES])
        q = rng.randrange(20, 400)
        at = (datetime.combine(start + timedelta(days=rng.randrange(0, 680)), datetime.min.time()) + timedelta(hours=9)).isoformat()
        receipts.append({"id": f"PR-{k + 1:05d}", "item": it["id"], "wh": wh, "qty": q, "at": at, "po": f"PO-{k + 1:05d}"})
        moves.append({"item": it["id"], "wh": wh, "qty": q, "at": at, "ref": f"PR-{k + 1:05d}"})

    closing = dict(stock)
    for m in moves:
        closing[(m["item"], m["wh"])] = closing.get((m["item"], m["wh"]), 0) + m["qty"]
    # seeded: the legacy WMS lost some movements, so its balance does not conserve against ERPNext's ledger
    legacy_drift = {}
    for (iid, wh), q in sorted(closing.items()):
        if wh in ("WH-CAI", "WH-ALX") and rng.random() < 0.06:
            legacy_drift[(iid, wh)] = q + rng.choice([-12, -5, 3, 40])
    # Saleor reports *available* (on hand less reserved) for WH-CAI/WH-GIZ
    reserved = {k: min(v, rng.randrange(0, 15)) for k, v in closing.items() if k[1] in ("WH-CAI", "WH-GIZ") and v > 0}

    return {
        "seed": SEED, "scale": scale, "customers": customers, "unmapped_customers": sorted(unmapped_customers), "items": items,
        "warehouses": [{"code": w, "region": r, "systems": s} for w, r, s in WAREHOUSES],
        "opening_stock": [{"item": i, "wh": w, "qty": q} for (i, w), q in sorted(stock.items())],
        "closing_stock": [{"item": i, "wh": w, "qty": q} for (i, w), q in sorted(closing.items())],
        "legacy_drift": [{"item": i, "wh": w, "qty": q} for (i, w), q in sorted(legacy_drift.items())],
        "reserved": [{"item": i, "wh": w, "qty": q} for (i, w), q in sorted(reserved.items())],
        "orders": orders, "deliveries": deliveries, "invoices": invoices, "payments": payments, "receipts": receipts, "moves": moves,
    }


class Enc(json.JSONEncoder):
    def default(self, o):
        return str(o) if isinstance(o, Decimal) else super().default(o)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--scale", type=int, default=3)
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    w = build(a.scale)
    (out / "world.json").write_text(json.dumps(w, cls=Enc, ensure_ascii=False, indent=1))
    print(json.dumps({k: len(v) for k, v in w.items() if isinstance(v, list)}))


if __name__ == "__main__":
    main()
