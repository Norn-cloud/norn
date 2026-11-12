"""Deterministic synthetic sales data for the data demo. No real data.

    python3 demo/data/generate.py OUTDIR [--orders N] [--seed S]

Writes into OUTDIR:

- `erpnext.json`, `shopify.json`, `odoo.json`: recorded snapshots in the exact
  shape of `registry/packs/<system>/synthetic.json`. Each carries parties,
  items, holdings and order lines, so they map through the admitted
  three-systems mappings (`norn dev --load`), and a sandbox ERP can be seeded
  with the same rows. The order set is split by channel: wholesale lines in
  ERPNext, online retail in Shopify, enterprise in Odoo.
- `orders.json`: the same order lines with the analytics columns the source
  schemas do not carry (order and ship dates, lead time, region, customer
  segment, unit price, currency). These load through the demo project's
  `sales.order.add` action.
- `manifest.json`: counts, the seed, and a SHA-256 of every file.

Quantities come in different units on purpose (g, kg and pieces). A sum
that is not grouped by unit adds grams to pieces; the demo groups by unit.
The ERP exports carry only the gram lines: the three-systems source schemas
declare `unit` as the code set {g}, so any other unit would be refused at
mapping, as it should be.
"""

import argparse
import hashlib
import json
import random
from datetime import date, timedelta
from pathlib import Path

REGIONS = ["north", "south", "east", "west"]
SEGMENTS = ["retail", "wholesale", "enterprise"]
CHANNEL_SYSTEM = {"wholesale": "erpnext", "retail": "shopify", "enterprise": "odoo"}
# code, label, unit, unit price (USD), typical quantity range
ITEMS = [
    ("GOLD", "Gold scrap", "g", "62.40", (5, 400)),
    ("SILVER", "Silver grain", "g", "0.85", (50, 5000)),
    ("COPPER", "Copper cathode", "kg", "9.30", (10, 2000)),
    ("CASE", "Watch case", "piece", "48.00", (1, 60)),
    ("CLASP", "Clasp set", "piece", "6.50", (5, 400)),
    ("RESIN", "Casting resin", "kg", "14.20", (1, 80)),
]
# How each system names a dataset and the record's identity and reference fields.
SHAPE = {
    "erpnext": {"id": "name", "ref": "custom_norn_ref", "party": "Party", "item": "Item", "holding": "Warehouse Lot Balance", "order": "Sales Order Item", "prefix": "SYN-ERPNEXT"},
    "shopify": {"id": "id", "ref": "metafield_norn_ref", "party": "Customer", "item": "ProductVariant", "holding": "InventoryLevel", "order": "OrderLine", "prefix": "SYN-SHOPIFY"},
    "odoo": {"id": "id", "ref": "x_norn_ref", "party": "res.partner", "item": "product.product", "holding": "stock.quant", "order": "sale.order.line", "prefix": "SYN-ODOO"},
}
START = date(2026, 1, 5)
# Holding cost per gram (USD), the same values the live sandboxes are seeded with.
STOCK_COST_PER_GRAM = {"GOLD": 45, "SILVER": 0.8}


def generate(orders: int, seed: int):
    rng = random.Random(seed)
    customers = []
    for index in range(1, 241):
        segment = SEGMENTS[index % 3] if index % 7 else "wholesale"
        customers.append({"ref": f"demo/party/customer-{index:03d}", "label": f"Customer {index:03d}", "segment": segment, "region": REGIONS[(index * 5) % 4]})
    rows = []
    for number in range(1, orders + 1):
        customer = customers[rng.randrange(len(customers))]
        code, label, unit, price, (low, high) = ITEMS[rng.randrange(len(ITEMS))]
        ordered = START + timedelta(days=rng.randrange(300))
        # Lead time depends on segment, region and size, with noise: something to model.
        base = {"retail": 2, "wholesale": 6, "enterprise": 11}[customer["segment"]] + REGIONS.index(customer["region"])
        quantity = rng.randint(low, high)
        lead = max(1, round(rng.gauss(base + quantity / high * 4, 1.5)))
        rows.append({
            "order_ref": f"demo/order/{number:05d}/line/1",
            "order_date": ordered.isoformat(),
            "ship_date": (ordered + timedelta(days=lead)).isoformat(),
            "lead_time_days": lead,
            "customer_ref": customer["ref"],
            "segment": customer["segment"],
            "region": customer["region"],
            "item_code": code,
            "quantity": str(quantity),
            "unit": unit,
            "unit_price": price,
            "currency": "USD",
            "system": CHANNEL_SYSTEM[customer["segment"]],
        })
    return customers, rows


def snapshot(system: str, customers, rows):
    shape = SHAPE[system]
    out = []

    def record(dataset, local, ref, **fields):
        out.append({"dataset": dataset, "record": {shape["id"]: f"{shape['prefix']}-{local}", shape["ref"]: ref, **fields}})

    for role in ("owner", "custodian"):
        record(shape["party"], role, f"demo/party/{role}", label=role.title(), role=role)
    # The three-systems source schemas declare `unit` as a code set of `g`
    # only, so the ERP exports carry the gram items; every unit is in orders.json.
    mine = [row for row in rows if row["system"] == system and row["unit"] == "g"]
    used = {row["customer_ref"] for row in mine}
    for customer in customers:
        if customer["ref"] in used:
            record(shape["party"], customer["ref"].rsplit("/", 1)[1], customer["ref"], label=customer["label"], role="customer")
    for code, label, unit, price, _ in ITEMS:
        if unit != "g":
            continue
        record(shape["item"], f"item-{code.lower()}", f"demo/item/{code.lower()}", code=code, label=label, sku=f"SKU-{code}", price=float(price))
        for region in REGIONS:
            record(shape["holding"], f"holding-{region}-{code.lower()}", f"demo/holding/owner/{region}/{code.lower()}", resource_code=code, quantity=100000, unit=unit, location=region, item_ref=f"demo/item/{code.lower()}", owner_ref="demo/party/owner", custodian_ref="demo/party/custodian", stock_cost=round(100000 * STOCK_COST_PER_GRAM[code]), cost_currency="USD", valuation_method="moving_average")
    for row in mine:
        number = row["order_ref"].split("/")[2]
        record(shape["order"], f"order-{number}", row["order_ref"], quantity=int(row["quantity"]), unit=row["unit"], due_at=f"{row['ship_date']}T12:00:00Z", provider_ref="demo/party/owner", receiver_ref=row["customer_ref"], holding_ref=f"demo/holding/owner/{row['region']}/{row['item_code'].lower()}")
    return {"profile": f"synthetic-{system}-three/1", "capability": "synthetic_snapshot", "productionCdc": False, "snapshotWatermark": len(out), "snapshot": out}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("outdir")
    parser.add_argument("--orders", type=int, default=3000)
    parser.add_argument("--seed", type=int, default=20261115)
    args = parser.parse_args()
    out = Path(args.outdir)
    out.mkdir(parents=True, exist_ok=True)
    customers, rows = generate(args.orders, args.seed)
    files = {f"{system}.json": snapshot(system, customers, rows) for system in SHAPE}
    files["orders.json"] = {"seed": args.seed, "rows": rows}
    digests = {}
    for name, document in files.items():
        text = json.dumps(document, indent=1, sort_keys=True) + "\n"
        (out / name).write_text(text)
        digests[name] = hashlib.sha256(text.encode()).hexdigest()
    manifest = {
        "seed": args.seed,
        "orders": len(rows),
        "customers": len({row["customer_ref"] for row in rows}),
        "perSystem": {system: sum(row["system"] == system and row["unit"] == "g" for row in rows) for system in SHAPE},
        "units": sorted({row["unit"] for row in rows}),
        "sha256": digests,
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    print(json.dumps(manifest))


if __name__ == "__main__":
    main()
