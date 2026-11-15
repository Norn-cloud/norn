# Seed an ERPNext sandbox with the demo's deterministic synthetic export (#921).
#
# Run inside the site's bench console, with the export copied into the container:
#   docker cp erpnext.json <backend>:/tmp/norn-erpnext.json
#   docker cp demo/sandboxes/erpnext/seed.py <backend>:/tmp/norn-seed.py
#   echo 'exec(open("/tmp/norn-seed.py").read(), {"__name__": "__main__"})' \
#     | docker exec -i <backend> bench --site <site> console
# (one exec, so the helpers see the module's globals; IPython reads piped
# statements one cell at a time.)
# The export is `python3 demo/data/generate.py OUT` -> OUT/erpnext.json, the
# registry/packs/erpnext/synthetic.json shape. Re-running is idempotent: every
# record is found by its canonical reference before it is created, and a found record
# whose values differ from the export is updated to them (the seed converges, so a
# sandbox seeded from an older export ends equal to the current one). With
# NORN_SEED_DRY_RUN=1 it only counts what it would create or change, and writes nothing.
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
import os

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

DRY = os.environ.get("NORN_SEED_DRY_RUN") == "1"
created, changed = {}, {}

def find(doctype, ref):
    return frappe.db.get_value(doctype, {"custom_norn_ref": ref}, "name")

def converge(doctype, name, values):
    """Set the VALUES that differ from the stored row's (floats to the cent); count the change."""
    stored = frappe.db.get_value(doctype, name, list(values), as_dict=True)
    def same(a, b):
        if isinstance(a, (int, float)) and isinstance(b, (int, float)):
            return round(float(a), 2) == round(float(b), 2)
        return str(a or "") == str(b or "")  # a stored date reads back as a date
    diff = {k: v for k, v in values.items() if not same(stored[k], v)}
    if diff:
        changed[doctype] = changed.get(doctype, 0) + 1
        if not DRY:
            frappe.db.set_value(doctype, name, diff)

def new(doctype):
    created[doctype] = created.get(doctype, 0) + 1
    return not DRY

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
    if name:
        converge("Item", name, {"item_name": record["label"], "standard_rate": record["price"], "custom_sku": record["sku"]})
    elif new("Item"):
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
    if name:
        converge("Customer", name, {"customer_name": record["label"], "custom_norn_role": record["role"]})
    elif new("Customer"):
        name = frappe.get_doc({
            "doctype": "Customer", "customer_name": record["label"], "customer_type": "Company",
            "customer_group": customer_group, "territory": territory,
            "custom_norn_ref": record["custom_norn_ref"], "custom_norn_role": record["role"],
        }).insert().name
    customers[record["custom_norn_ref"]] = name

holdings = {}
for record in by.get("Warehouse Lot Balance", []):
    # The export carries a balance once per change; its last row is the balance now.
    holdings[record["custom_norn_ref"]] = record
for ref, record in holdings.items():
    values = {k: record[k] for k in ("resource_code", "quantity", "unit", "location", "item_ref", "owner_ref", "custodian_ref")}
    values.update({"stock_cost": record.get("stock_cost", round(record["quantity"] * COST_PER_GRAM[record["item_ref"]], 2)),
                   "cost_currency": record.get("cost_currency", "USD"), "valuation_method": record.get("valuation_method", "moving_average")})
    name = find("Warehouse Lot Balance", ref)
    if name:
        converge("Warehouse Lot Balance", name, values)
    elif new("Warehouse Lot Balance"):
        frappe.get_doc({"doctype": "Warehouse Lot Balance", "custom_norn_ref": ref, **values}).insert()

existing = {row.custom_norn_ref: row for row in frappe.get_all(
    "Sales Order Item", filters={"custom_norn_ref": ["is", "set"]}, fields=["name", "parent", "custom_norn_ref"])}
for record in by.get("Sales Order Item", []):
    holding = holdings[record["holding_ref"]]
    due = record["due_at"][:10]
    row = existing.get(record["custom_norn_ref"])
    if row:
        converge("Sales Order", row.parent, {"customer": customers[record["receiver_ref"]]})
        converge("Sales Order Item", row.name, {
            "item_code": items[holding["item_ref"]], "qty": record["quantity"], "delivery_date": due,
            "custom_provider_ref": record["provider_ref"], "custom_receiver_ref": record["receiver_ref"],
            "custom_holding_ref": record["holding_ref"], "custom_due_at": record["due_at"]})
        continue
    if not new("Sales Order Item"):
        continue
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

if DRY:
    frappe.db.rollback()
    print("dry run: would create", created, "and change", changed)
    raise SystemExit(0)
print("created", created, "changed", changed)

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
