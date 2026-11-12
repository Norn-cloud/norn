# Seed an ERPNext sandbox with the demo's deterministic synthetic export (#921).
#
# Run inside the site's bench console, with the export copied into the container:
#   docker cp erpnext.json <backend>:/tmp/norn-erpnext.json
#   docker exec -i <backend> bench --site <site> console < demo/sandboxes/erpnext/seed.py
# The export is `python3 demo/data/generate.py OUT` -> OUT/erpnext.json, the
# registry/packs/erpnext/synthetic.json shape. Re-running is idempotent: every
# record is found by its canonical reference before it is created.
#
# What lands where (native ERPNext records wherever the export has a native meaning):
#   Party                 -> Customer (custom_norn_ref, custom_norn_role)
#   Item                  -> Item (item_code, item_name, standard_rate; custom_norn_ref, custom_sku)
#   Sales Order Item      -> a draft Sales Order with one line (qty in the UOM `g`, delivery_date;
#                            custom_norn_ref and the role references on the line)
#   Warehouse Lot Balance -> the custom DocType "Warehouse Lot Balance": owner- and
#                            location-partitioned balances have no native ERPNext record.
# The canonical references (`custom_*`) are the reviewed fixture extensions the
# three-systems contract names; ERPNext's own names stay the source identities.
# The last step creates a read-only API user and prints only its user id.
import json

import frappe
from frappe.custom.doctype.custom_field.custom_field import create_custom_fields

EXPORT = "/tmp/norn-erpnext.json"
READER = "norn-reader@sandbox.invalid"
COST_PER_GRAM = {"demo/item/gold": 45, "demo/item/silver": 0.8}

rows = json.load(open(EXPORT))["snapshot"]
by = {}
for row in rows:
    by.setdefault(row["dataset"], []).append(row["record"])

data = lambda name, label: {"fieldname": name, "label": label, "fieldtype": "Data", "read_only": 0}
create_custom_fields({
    "Customer": [data("custom_norn_ref", "Norn reference"), data("custom_norn_role", "Norn role")],
    "Item": [data("custom_norn_ref", "Norn reference"), data("custom_sku", "SKU")],
    "Sales Order Item": [
        data("custom_norn_ref", "Norn reference"), data("custom_provider_ref", "Provider reference"),
        data("custom_receiver_ref", "Receiver reference"), data("custom_holding_ref", "Holding reference"),
        data("custom_due_at", "Due at (UTC)"),
    ],
}, update=True)

if not frappe.db.exists("DocType", "Warehouse Lot Balance"):
    fields = [
        ("custom_norn_ref", "Data", "Norn reference"), ("resource_code", "Data", "Resource code"),
        ("quantity", "Float", "Quantity"), ("unit", "Data", "Unit"), ("location", "Data", "Location"),
        ("item_ref", "Data", "Item reference"), ("owner_ref", "Data", "Owner reference"),
        ("custodian_ref", "Data", "Custodian reference"), ("stock_cost", "Float", "Stock cost"),
        ("cost_currency", "Data", "Cost currency"), ("valuation_method", "Data", "Valuation method"),
    ]
    frappe.get_doc({
        "doctype": "DocType", "name": "Warehouse Lot Balance", "module": "Stock", "custom": 1,
        "autoname": "format:WLB-{#####}", "track_changes": 1,
        "fields": [{"fieldname": f, "fieldtype": t, "label": l, "in_list_view": 1 if i < 4 else 0} for i, (f, t, l) in enumerate(fields)],
        "permissions": [{"role": r, "read": 1, "write": 1 if r == "System Manager" else 0, "create": 1 if r == "System Manager" else 0} for r in ("System Manager", "Stock User")],
    }).insert()

def find(doctype, ref):
    return frappe.db.get_value(doctype, {"custom_norn_ref": ref}, "name")

company = frappe.defaults.get_global_default("company") or frappe.get_all("Company", pluck="name")[0]
warehouse = frappe.get_all("Warehouse", filters={"company": company, "is_group": 0}, pluck="name", order_by="name")[0]

# The export's unit is the code `g`; ERPNext keeps it as its own UOM record.
if not frappe.db.exists("UOM", "g"):
    frappe.get_doc({"doctype": "UOM", "uom_name": "g", "must_be_whole_number": 0}).insert()
for name in frappe.get_all("Item", filters={"custom_norn_ref": ["is", "set"], "stock_uom": ["!=", "g"]}, pluck="name"):
    frappe.db.set_value("Item", name, "stock_uom", "g")
for name in frappe.get_all("Sales Order Item", filters={"custom_norn_ref": ["is", "set"], "uom": ["!=", "g"]}, pluck="name"):
    frappe.db.set_value("Sales Order Item", name, {"uom": "g", "stock_uom": "g"})

items = {}
for record in by.get("Item", []):
    name = find("Item", record["custom_norn_ref"])
    if not name:
        name = frappe.get_doc({
            "doctype": "Item", "item_code": record["code"], "item_name": record["label"], "item_group": "Products",
            "stock_uom": "g", "is_stock_item": 1, "standard_rate": record["price"],
            "custom_norn_ref": record["custom_norn_ref"], "custom_sku": record["sku"],
        }).insert().name
    items[record["custom_norn_ref"]] = name

leaf = lambda doctype: frappe.get_all(doctype, filters={"is_group": 0}, pluck="name", order_by="name")[0]
customer_group, territory = leaf("Customer Group"), leaf("Territory")
customers = {}
for record in by.get("Party", []):
    name = find("Customer", record["custom_norn_ref"])
    if not name:
        name = frappe.get_doc({
            "doctype": "Customer", "customer_name": record["label"], "customer_type": "Company",
            "customer_group": customer_group, "territory": territory,
            "custom_norn_ref": record["custom_norn_ref"], "custom_norn_role": record["role"],
        }).insert().name
    customers[record["custom_norn_ref"]] = name

holdings = {}
for record in by.get("Warehouse Lot Balance", []):
    holdings[record["custom_norn_ref"]] = record
    if not find("Warehouse Lot Balance", record["custom_norn_ref"]):
        cost = record["quantity"] * COST_PER_GRAM[record["item_ref"]]
        frappe.get_doc({
            "doctype": "Warehouse Lot Balance", **{k: record[k] for k in (
                "custom_norn_ref", "resource_code", "quantity", "unit", "location", "item_ref", "owner_ref", "custodian_ref")},
            "stock_cost": round(cost, 2), "cost_currency": "USD", "valuation_method": "moving_average",
        }).insert()

existing = set(frappe.get_all("Sales Order Item", filters={"custom_norn_ref": ["is", "set"]}, pluck="custom_norn_ref"))
for record in by.get("Sales Order Item", []):
    if record["custom_norn_ref"] in existing:
        continue
    holding = holdings[record["holding_ref"]]
    due = record["due_at"][:10]
    frappe.get_doc({
        "doctype": "Sales Order", "company": company, "customer": customers[record["receiver_ref"]],
        "transaction_date": due, "delivery_date": due, "set_warehouse": warehouse,
        "items": [{
            "item_code": items[holding["item_ref"]], "qty": record["quantity"], "uom": "g",
            "delivery_date": due, "warehouse": warehouse,
            "custom_norn_ref": record["custom_norn_ref"], "custom_provider_ref": record["provider_ref"],
            "custom_receiver_ref": record["receiver_ref"], "custom_holding_ref": record["holding_ref"],
            "custom_due_at": record["due_at"],
        }],
    }).insert()

if not frappe.db.exists("Role", "Norn Reader"):
    frappe.get_doc({"doctype": "Role", "role_name": "Norn Reader", "desk_access": 0}).insert()
for doctype in ("Customer", "Item", "Sales Order", "Warehouse Lot Balance"):
    if not frappe.db.exists("Custom DocPerm", {"parent": doctype, "role": "Norn Reader"}):
        from frappe.permissions import add_permission
        add_permission(doctype, "Norn Reader", 0)
if not frappe.db.exists("User", READER):
    frappe.get_doc({"doctype": "User", "email": READER, "first_name": "Norn", "last_name": "Reader",
                    "user_type": "System User", "send_welcome_email": 0, "roles": [{"role": "Norn Reader"}]}).insert()
frappe.db.commit()
print("seeded", {k: len(v) for k, v in by.items()}, "reader", READER)
