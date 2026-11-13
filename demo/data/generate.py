"""Deterministic synthetic sales data for the data demo. No real data.

    python3 demo/data/generate.py OUTDIR [--orders N] [--seed S]

Writes into OUTDIR:

- `erpnext.json`, `shopify.json`, `odoo.json`: recorded snapshots in the exact
  shape of `registry/packs/<system>/synthetic.json`. Each carries parties,
  items, holdings and order lines, so they map through the admitted
  three-systems mappings (`norn dev --load`), and a sandbox ERP can be seeded
  with the same rows. The order set is split by channel: wholesale lines in
  ERPNext, online retail in Shopify, enterprise in Odoo.
  Each shipped gram line also has its delivery, the delivery's two ledger
  postings (cost of goods sold and the matching inventory credit, in the
  USD stock book) and the customer's payment, in the currency of the
  customer's region at that month's rate; a few are later refunded. So
  every canon type Studio shows (holdings, order lines, deliveries,
  postings, settlements) has ten months of records.
- `orders.json`: the same order lines with the analytics columns the source
  schemas do not carry (order and ship dates, lead time, region, customer
  segment, unit price, currency). These load through the demo project's
  `sales.order.add` action.
- `manifest.json`: counts, the seed, each holding's stock flow (closing =
  opening + receipts - deliveries, in grams) and a SHA-256 of every file.

Holdings move over time: each holding opens at START, and every receipt and
delivery emits its new balance as a later change of the same record, with
the change time in the system's own field (ERPNext `modified`, Shopify
`updated_at`, Odoo `write_date`) and a `sourceVersion` that orders the
changes, so each change commits as the holding's next version.

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
# The datasets each system exports deliveries, ledger lines and payments in.
SHAPE_FLOWS = {
    "erpnext": {"delivery": "Delivery Note Item", "posting": "GL Entry", "settlement": "Payment Entry"},
    "shopify": {"delivery": "FulfillmentLine", "posting": None, "settlement": "Transaction"},
    "odoo": {"delivery": "stock.move", "posting": "account.move.line", "settlement": "account.payment"},
}
# Customers pay in their region's currency; units of that currency per USD,
# drifting a little each month so a converted total has a trend.
REGION_CURRENCY = {"north": "EUR", "south": "EGP", "east": "AED", "west": "USD"}
USD_RATE = {"EUR": 0.92, "EGP": 48.5, "AED": 3.6725, "USD": 1.0}
SHAPE = {
    "erpnext": {"id": "name", "ref": "custom_norn_ref", "party": "Party", "item": "Item", "holding": "Warehouse Lot Balance", "order": "Sales Order Item", "prefix": "SYN-ERPNEXT", "observed": "modified"},
    "shopify": {"id": "id", "ref": "metafield_norn_ref", "party": "Customer", "item": "ProductVariant", "holding": "InventoryLevel", "order": "OrderLine", "prefix": "SYN-SHOPIFY", "observed": "updated_at"},
    "odoo": {"id": "id", "ref": "x_norn_ref", "party": "res.partner", "item": "product.product", "holding": "stock.quant", "order": "sale.order.line", "prefix": "SYN-ODOO", "observed": "write_date"},
}
START = date(2026, 1, 5)
# Holding cost per gram (USD), the same values the live sandboxes are seeded with.
STOCK_COST_PER_GRAM = {"GOLD": 45, "SILVER": 0.8}
# Grams per holding: opening balance, reorder point and receipt size. A
# receipt restocks a holding before a delivery would take it below the
# reorder point.
STOCK_PLAN = {"GOLD": (5000, 1000, 5000), "SILVER": (20000, 5000, 20000)}


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

    def record(dataset, local, ref, version=None, **fields):
        row = {"dataset": dataset, "record": {shape["id"]: f"{shape['prefix']}-{local}", shape["ref"]: ref, **fields}}
        if version is not None:
            row["sourceVersion"] = version
        out.append(row)

    for role in ("owner", "custodian"):
        record(shape["party"], role, f"demo/party/{role}", label=role.title(), role=role)
    # The three-systems source schemas declare `unit` as a code set of `g`
    # only, so the ERP exports carry the gram items; every unit is in orders.json.
    mine = [row for row in rows if row["system"] == system and row["unit"] == "g"]
    used = {row["customer_ref"] for row in mine}
    for customer in customers:
        if customer["ref"] in used:
            record(shape["party"], customer["ref"].rsplit("/", 1)[1], customer["ref"], label=customer["label"], role="customer")
    # Each holding's balance over time: the opening balance at START, then
    # its new balance after every receipt (+) and delivery (-), each a
    # separate change with its own time and source version.
    def holding(code, region, quantity, at, version):
        record(shape["holding"], f"holding-{region}-{code.lower()}", f"demo/holding/owner/{region}/{code.lower()}", version, resource_code=code, quantity=quantity, unit="g", location=region, item_ref=f"demo/item/{code.lower()}", owner_ref="demo/party/owner", custodian_ref="demo/party/custodian", stock_cost=round(quantity * STOCK_COST_PER_GRAM[code]), cost_currency="USD", valuation_method="moving_average", **{shape["observed"]: at})

    opened = f"{START.isoformat()}T00:00:00Z"
    balance = {}
    for code, label, unit, price, _ in ITEMS:
        if unit != "g":
            continue
        record(shape["item"], f"item-{code.lower()}", f"demo/item/{code.lower()}", code=code, label=label, sku=f"SKU-{code}", price=float(price))
        for region in REGIONS:
            balance[(code, region)] = STOCK_PLAN[code][0]
            holding(code, region, STOCK_PLAN[code][0], opened, f"{opened}#000000")
    for row in mine:
        number = row["order_ref"].split("/")[2]
        record(shape["order"], f"order-{number}", row["order_ref"], quantity=int(row["quantity"]), unit=row["unit"], due_at=f"{row['ship_date']}T12:00:00Z", provider_ref="demo/party/owner", receiver_ref=row["customer_ref"], holding_ref=f"demo/holding/owner/{row['region']}/{row['item_code'].lower()}")
    flows = {key: {"receipts": 0, "deliveries": 0} for key in balance}
    sequence = 0
    for row in sorted(mine, key=lambda row: (row["ship_date"], row["order_ref"])):
        key = (row["item_code"], row["region"])
        quantity = int(row["quantity"])
        _, reorder, receipt = STOCK_PLAN[row["item_code"]]
        while balance[key] - quantity < reorder:
            at = f"{row['ship_date']}T08:00:00Z"
            balance[key] += receipt
            flows[key]["receipts"] += receipt
            sequence += 1
            holding(key[0], key[1], balance[key], at, f"{at}#{sequence:06d}")
        at = f"{row['ship_date']}T12:00:00Z"
        balance[key] -= quantity
        flows[key]["deliveries"] += quantity
        sequence += 1
        holding(key[0], key[1], balance[key], at, f"{at}#{sequence:06d}")
    flows_out = SHAPE_FLOWS[system]
    for row in mine:
        number = row["order_ref"].split("/")[2]
        delivery = f"demo/delivery/{number}/line/1"
        shipped = f"{row['ship_date']}T12:00:00Z"
        record(flows_out["delivery"], f"delivery-{number}", delivery, quantity=int(row["quantity"]), unit="g", occurred_at=shipped, provider_ref="demo/party/owner", receiver_ref=row["customer_ref"], holding_ref=f"demo/holding/owner/{row['region']}/{row['item_code'].lower()}", order_ref=row["order_ref"], posting_state="posted")
        cost = round(int(row["quantity"]) * STOCK_COST_PER_GRAM[row["item_code"]], 2)
        if flows_out["posting"] is not None:
            for role, sign in (("cogs", 1), ("inventory", -1)):
                record(flows_out["posting"], f"gl-{number}-{role}", f"demo/posting/{number}/{role}", account_code=role, amount=sign * cost, currency="USD", book="demo/stock-usd", account_role=role, event_ref=delivery, posting_state="posted")
        currency = REGION_CURRENCY[row["region"]]
        month = int(row["ship_date"][5:7])
        rate = USD_RATE[currency] * (1 + 0.004 * (month - 6)) if currency != "USD" else 1.0
        paid = round(int(row["quantity"]) * float(row["unit_price"]) * rate, 2)
        record(flows_out["settlement"], f"pay-{number}", f"demo/settlement/{number}/payment", amount=paid, currency=currency, kind="payment", payer_ref=row["customer_ref"], payee_ref="demo/party/owner")
        # A quarter of the price is refunded on about one line in 25, chosen by the order's ref.
        if int(hashlib.sha256(row["order_ref"].encode()).hexdigest(), 16) % 25 == 0:
            record(flows_out["settlement"], f"refund-{number}", f"demo/settlement/{number}/refund", amount=round(paid * 0.25, 2), currency=currency, kind="refund", payer_ref="demo/party/owner", payee_ref=row["customer_ref"])
    for (code, region), flow in flows.items():
        assert balance[(code, region)] == STOCK_PLAN[code][0] + flow["receipts"] - flow["deliveries"] >= 0
    stock = {f"{code.lower()}/{region}": {"opening": STOCK_PLAN[code][0], **flow, "closing": balance[(code, region)]} for (code, region), flow in sorted(flows.items())}
    return {"profile": f"synthetic-{system}-three/1", "capability": "synthetic_snapshot", "productionCdc": False, "snapshotWatermark": len(out), "snapshot": out}, stock


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("outdir")
    parser.add_argument("--orders", type=int, default=3000)
    parser.add_argument("--seed", type=int, default=20261115)
    args = parser.parse_args()
    out = Path(args.outdir)
    out.mkdir(parents=True, exist_ok=True)
    customers, rows = generate(args.orders, args.seed)
    files, stock = {}, {}
    for system in SHAPE:
        files[f"{system}.json"], stock[system] = snapshot(system, customers, rows)
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
        # Grams per holding: closing = opening + receipts - deliveries.
        "stock": stock,
        "sha256": digests,
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    print(json.dumps(manifest))


if __name__ == "__main__":
    main()
