# Seed an Odoo sandbox with the demo's deterministic synthetic export (#921).
#
# Run in the database's Odoo shell, with the export copied into the container:
#   docker cp odoo.json <odoo>:/tmp/norn-odoo.json
#   docker exec -i <odoo> odoo shell -d <db> --no-http < demo/sandboxes/odoo/seed.py
# The export is `python3 demo/data/generate.py OUT` -> OUT/odoo.json, the
# registry/packs/odoo/synthetic.json shape. Re-running is idempotent: every
# record is found by its canonical reference before it is created.
#
# What lands where (native Odoo records and relations wherever the export has them):
#   res.partner     -> res.partner (name; x_norn_ref, x_norn_role)
#   product.product -> a storable product in the UoM `g` (default_code, barcode = sku,
#                      list_price; x_norn_ref)
#   stock.quant     -> a quant per item and location under WH/Stock, owned by the
#                      owner partner (owner_id); x_norn_ref, x_custodian_ref and the
#                      cost fields the export's valuation profile declares
#   sale.order.line -> a draft sale order for the receiver with one line
#                      (product_uom_qty in g; x_norn_ref, x_provider_ref, x_holding_ref, x_due_at)
# The x_ fields are the reviewed fixture extensions the three-systems contract
# names; Odoo's own ids stay the source identities. The last step creates a
# read-only user with an API key; the key goes to the file named by
# NORN_KEY_FILE (owner-only), never to the output.
import json
import os
from datetime import timedelta

from odoo import fields

EXPORT = "/tmp/norn-odoo.json"
READER = "norn-reader"
COST_PER_GRAM = {"demo/item/gold": 45, "demo/item/silver": 0.8}

rows = json.load(open(EXPORT))["snapshot"]
by = {}
for row in rows:
    by.setdefault(row["dataset"], []).append(row["record"])

def custom(model, name, label, ttype="char"):
    model_id = env["ir.model"]._get_id(model)
    if not env["ir.model.fields"].search([("model", "=", model), ("name", "=", name)]):
        env["ir.model.fields"].create({"model_id": model_id, "name": name, "field_description": label, "ttype": ttype, "state": "manual"})

for model in ("res.partner", "product.product", "stock.quant", "sale.order.line"):
    custom(model, "x_norn_ref", "Norn reference")
custom("res.partner", "x_norn_role", "Norn role")
custom("stock.quant", "x_custodian_ref", "Custodian reference")
custom("stock.quant", "x_stock_cost", "Stock cost", "float")
custom("stock.quant", "x_cost_currency", "Cost currency")
custom("stock.quant", "x_valuation_method", "Valuation method")
for name, label in (("x_provider_ref", "Provider reference"), ("x_holding_ref", "Holding reference"), ("x_due_at", "Due at (UTC)")):
    custom("sale.order.line", name, label)
env.cr.commit()
env.invalidate_all()

def find(model, ref):
    return env[model].search([("x_norn_ref", "=", ref)], limit=1)

partners = {}
for record in by.get("res.partner", []):
    partner = find("res.partner", record["x_norn_ref"]) or env["res.partner"].create({
        "name": record["label"], "is_company": True, "x_norn_ref": record["x_norn_ref"], "x_norn_role": record["role"]})
    partners[record["x_norn_ref"]] = partner

gram = env.ref("uom.product_uom_gram")
products = {}
for record in by.get("product.product", []):
    product = find("product.product", record["x_norn_ref"])
    if not product:
        template = env["product.template"].create({
            "name": record["label"], "default_code": record["code"], "barcode": record["sku"],
            "list_price": record["price"], "type": "consu", "is_storable": True, "uom_id": gram.id})
        product = template.product_variant_id
        product.x_norn_ref = record["x_norn_ref"]
    products[record["x_norn_ref"]] = product

stock = env["stock.warehouse"].search([], limit=1).lot_stock_id
def location(name):
    found = env["stock.location"].search([("name", "=", name), ("location_id", "=", stock.id)], limit=1)
    return found or env["stock.location"].create({"name": name, "location_id": stock.id, "usage": "internal"})

holdings = {}
for record in by.get("stock.quant", []):
    holdings[record["x_norn_ref"]] = record
    if find("stock.quant", record["x_norn_ref"]):
        continue
    product, place, owner = products[record["item_ref"]], location(record["location"]), partners[record["owner_ref"]]
    env["stock.quant"]._update_available_quantity(product, place, record["quantity"], owner_id=owner)
    quant = env["stock.quant"].search([("product_id", "=", product.id), ("location_id", "=", place.id), ("owner_id", "=", owner.id)], limit=1)
    quant.write({
        "x_norn_ref": record["x_norn_ref"], "x_custodian_ref": record["custodian_ref"],
        "x_stock_cost": round(record["quantity"] * COST_PER_GRAM[record["item_ref"]], 2),
        "x_cost_currency": "USD", "x_valuation_method": "moving_average"})

existing = set(env["sale.order.line"].search([("x_norn_ref", "!=", False)]).mapped("x_norn_ref"))
for record in by.get("sale.order.line", []):
    if record["x_norn_ref"] in existing:
        continue
    holding = holdings[record["holding_ref"]]
    env["sale.order"].create({
        "partner_id": partners[record["receiver_ref"]].id,
        "commitment_date": fields.Datetime.to_datetime(record["due_at"][:19].replace("T", " ")),
        "order_line": [(0, 0, {
            "product_id": products[holding["item_ref"]].id, "product_uom_qty": record["quantity"],
            "product_uom_id": gram.id, "x_norn_ref": record["x_norn_ref"], "x_provider_ref": record["provider_ref"],
            "x_holding_ref": record["holding_ref"], "x_due_at": record["due_at"]})],
    })
env.cr.commit()

reader = env["res.users"].search([("login", "=", READER)], limit=1) or env["res.users"].create({
    "name": "Norn Reader", "login": READER,
    "group_ids": [(6, 0, [env.ref(x).id for x in ("base.group_user", "sales_team.group_sale_salesman_all_leads", "stock.group_stock_user")])]})
key_file = os.environ.get("NORN_KEY_FILE")
if key_file:
    key = env["res.users.apikeys"].with_user(reader).sudo()._generate(None, "norn-sync", fields.Datetime.now() + timedelta(days=90))
    fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, key.encode())
    os.close(fd)
env.cr.commit()
print("seeded", {k: len(v) for k, v in by.items()}, "reader", READER)
