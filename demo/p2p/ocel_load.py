"""Map an OCEL 2.0 procure-to-pay log (JSON) to a recorded snapshot for the p2p pack.

    python3 demo/p2p/ocel_load.py OCEL_JSON OUT_JSON [--limit-events N] [--report REPORT_JSON]

Reads the OCEL 2.0 P2P sample (ocel-standard.org, Zenodo record 8412920) and
writes `{ source, mapping, snapshot }`, the recorded-export shape that
`norn dev --load` and `POST /v1/observations:load` take (see
crates/norn-cell-do/src/recorded.rs and registry/packs/p2p/p2p.ts).

Mapping (and what does not map losslessly; the report counts each):
- OCEL objects become one row per object in the dataset named by its type.
  OCEL keeps each attribute as a list of (time, value); this export gives
  every attribute's real value at 1970-01-01 (the "initial" value) and later
  entries with an empty value only. The row takes the first non-empty value;
  the empty timed entries are counted and dropped (attribute-over-time).
- Object-to-object relations become link keys on the row, named after the
  qualifier (purchase_order, invoice, ...). The direction is Norn's: a link
  is declared on the many side.
- Each object's time is the time of the event that created it (Create
  Purchase Order, Create Goods Receipt, ...): the valid time.
- Events become `event` rows; each event-to-object relation an `e2o` row with
  its qualifier.
- An order line's amount is quantity times net price (the log states none),
  and every amount is typed USD (the log carries no currency).
"""

import argparse
import collections
import json
import sys
from decimal import Decimal

SOURCE = "ocel2-p2p"
MAPPING = "p2p.ocel.mapping"

# OCEL attribute name -> row field, per object type.
ATTRIBUTES = {
    "purchase_requisition": {"Purchasing Group (EBAN-EKGRP)": "purchasing_group", "Release Indicator (EBAN-FRGZU)": "release_indicator"},
    "quotation": {"Purchasing Group (EBAN-EKGRP)": "purchasing_group", "RFQ Type (EBAN-BSART)": "rfq_type",
                  "Purchasing Organization (EBAN-EKORG)": "purchasing_org", "Vendor (EBAN-LIFNR)": "vendor"},
    "purchase_order": {"Purchasing Group (EKKO-EKGRP)": "purchasing_group", "Purchasing Organization (EKKO-EKORG)": "purchasing_org",
                       "Document Type (EKKO-BSART)": "document_type", "Release Status (EKKO-FRGZU)": "release_status", "Vendor (EKKO-LIFNR)": "vendor"},
    "material": {"Net Price (EKPO-NETPR)": "net_price", "Quantity (EKPO-MENGE)": "quantity", "Delivery Date (EKPO-BEDAT)": "delivery_date",
                 "Plant (EKPO-WERKS)": "plant", "Material (EKPO-MATNR)": "material", "Storage Location (EKPO-LGORT)": "storage_location"},
    "invoice receipt": {"Debit Amount (BSEG-DMBTR)": "amount", "Credit Amount (BSEG-WRBTR)": "credit_amount", "Payment Block (RSEG-ZLSPR)": "payment_block"},
    "goods receipt": {"Movement Type (MSEG-BWART)": "movement_type", "Invoice Receipt (MSEG-WEAHR)": "invoice_expected", "Posting Date (MKPF-BLDAT)": "posting_date"},
    "payment": {"Payment Method (ZLSCH)": "method", "Amount (DMBTR)": "amount"},
}
# Attributes the export carries but the pack does not keep: always empty.
EMPTY = {"new_value"}

# O2O qualifier -> (holder type, link field, direction). "out": the holder
# names the target; "in": the target names the holder (the many side).
O2O = {
    "Quotation of PR": ("in", "requisition"),            # PR -> quotation: quotation.requisition = PR
    "Purchase Order of Quotation": ("in", "quotation"),   # quotation -> PO: PO.quotation
    "Materials of Purchase Order": ("in", "purchase_order"),
    "assigned_materials of PR": ("in", "requisition"),
    "Materials of Quotation": ("in", "quotation"),
    "Invoice Receipts of Purchase Order": ("in", "purchase_order"),
    "Good Receipts of Purchase Order": ("out", "purchase_order"),  # GR -> PO: GR.purchase_order
    "Invoice Receipt of Goods Receipt": ("out", "invoice"),
    "goods_receipt_pm": ("in", "goods_receipt"),          # GR -> payment: payment.goods_receipt
    "order_pm": ("in", "purchase_order"),                 # PO -> payment: payment.purchase_order
    "invoice_receipt_pm": ("in", "invoice"),              # invoice -> payment: payment.invoice
}
# Which event creates each object type: its time is the object's valid time.
CREATED_BY = {
    "purchase_requisition": ("Create Purchase Requisition", "created_at"),
    "quotation": ("Create Request for Quotation", "created_at"),
    "purchase_order": ("Create Purchase Order", "created_at"),
    "invoice receipt": ("Create Invoice Receipt", "received_at"),
    "goods receipt": ("Create Goods Receipt", "received_at"),
    "payment": ("Execute Payment", "paid_at"),
}
DATASET_ORDER = ["purchase_requisition", "quotation", "purchase_order", "material", "invoice receipt", "goods receipt", "payment", "event", "e2o"]
DECIMAL_FIELDS = {"net_price", "quantity", "amount", "credit_amount"}


def iso(text):
    """An OCEL time or SAP date string as an RFC 3339 UTC timestamp."""
    if text in (None, ""):
        return None
    text = str(text).strip().replace(" ", "T")
    if text.endswith("Z"):
        text = text[:-1]
    date, _, clock = text.partition("T")
    clock = (clock or "00:00:00").split(".")[0]
    if clock.count(":") == 1:
        clock += ":00"
    return f"{date}T{clock}Z"


def decimal_text(value, places):
    return str(Decimal(str(value)).quantize(Decimal(1).scaleb(-places)))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("ocel")
    parser.add_argument("out")
    parser.add_argument("--limit-events", type=int, default=0, help="keep only the first N events (by time) and the objects they touch")
    parser.add_argument("--report")
    args = parser.parse_args()
    log = json.load(open(args.ocel))
    report = collections.Counter()

    events = sorted(log["events"], key=lambda e: (e["time"], e["id"]))
    if args.limit_events:
        events = events[: args.limit_events]
    touched = {r["objectId"] for e in events for r in e.get("relationships", [])}
    objects = {o["id"]: o for o in log["objects"] if not args.limit_events or o["id"] in touched}

    rows = {o["id"]: {"id": o["id"]} for o in objects.values()}
    for o in objects.values():
        names = ATTRIBUTES.get(o["type"], {})
        for attribute in o.get("attributes", []):
            report["attribute_values"] += 1
            name, value, time = attribute["name"], attribute["value"], attribute["time"]
            if name in EMPTY:
                report["attribute_values_dropped_always_empty"] += 1
                continue
            field = names.get(name)
            if field is None:
                report[f"attribute_unmapped:{o['type']}:{name}"] += 1
                continue
            if value in ("", None):
                report["attribute_values_dropped_empty_timed"] += 1
                continue
            if field in rows[o["id"]]:
                report["attribute_values_dropped_later_nonempty"] += 1
                continue
            if not time.startswith("1970-01-01"):
                report["attribute_values_real_time"] += 1
            rows[o["id"]][field] = value
        for relation in o.get("relationships", []):
            report["o2o"] += 1
            rule = O2O.get(relation["qualifier"])
            target = relation["objectId"]
            if rule is None or target not in rows:
                report[f"o2o_unmapped:{relation['qualifier']}"] += 1
                continue
            direction, field = rule
            holder, value = (rows[target], o["id"]) if direction == "in" else (rows[o["id"]], target)
            if holder.get(field) not in (None, value):
                report[f"o2o_second_target_dropped:{relation['qualifier']}"] += 1
                continue
            holder[field] = value

    out_events, e2o = [], []
    for event in events:
        attrs = {a["name"]: a["value"] for a in event.get("attributes", [])}
        out_events.append({"id": event["id"], "activity": event["type"], "occurred_at": iso(event["time"]),
                           "resource": attrs.get("resource"), "lifecycle": attrs.get("lifecycle")})
        for n, relation in enumerate(event.get("relationships", [])):
            target = objects.get(relation["objectId"])
            e2o.append({"id": f"{event['id']}#{n}", "event": event["id"], "object_id": relation["objectId"],
                        "object_type": target["type"] if target else "unknown", "qualifier": relation["qualifier"]})
            report["e2o"] += 1
            if target is None:
                continue
            created = CREATED_BY.get(target["type"])
            if created and created[0] == event["type"]:
                row = rows[target["id"]]
                if created[1] in row:
                    report[f"creation_event_repeated:{target['type']}"] += 1
                else:
                    row[created[1]] = iso(event["time"])
            if target["type"] == "purchase_order" and event["type"] == "Approve Purchase Order":
                rows[target["id"]].setdefault("approved_at", iso(event["time"]))

    # Typed values, the invoice's vendor (from its order) and each line's amount.
    for o in objects.values():
        row = rows[o["id"]]
        if o["type"] == "material":
            quantity, price = Decimal(str(row.get("quantity", 0))), Decimal(str(row.get("net_price", 0)))
            row["quantity"], row["net_price"] = decimal_text(quantity, 3), decimal_text(price, 2)
            row["amount"] = decimal_text(quantity * price, 2)
            row["delivery_date"] = iso(row.get("delivery_date"))
        for field in ("amount", "credit_amount"):
            if o["type"] != "material" and row.get(field) not in (None, ""):
                row[field] = decimal_text(row[field], 2)
        if o["type"] == "goods receipt":
            row["posting_date"] = iso(row.get("posting_date"))
    for o in objects.values():
        if o["type"] == "invoice receipt":
            order = rows.get(rows[o["id"]].get("purchase_order") or "")
            rows[o["id"]]["vendor"] = order.get("vendor") if order else None

    by_dataset = collections.defaultdict(list)
    for o in objects.values():
        by_dataset[o["type"]].append(rows[o["id"]])
    by_dataset["event"], by_dataset["e2o"] = out_events, e2o
    snapshot = [{"dataset": d, "record": r} for d in DATASET_ORDER for r in by_dataset.get(d, [])]
    json.dump({"source": SOURCE, "mapping": MAPPING, "connectorInstance": "zenodo-8412920", "snapshot": snapshot},
              open(args.out, "w"), separators=(",", ":"))
    summary = {"rows": len(snapshot), "by_dataset": {d: len(by_dataset.get(d, [])) for d in DATASET_ORDER}, "fidelity": dict(sorted(report.items()))}
    if args.report:
        json.dump(summary, open(args.report, "w"), indent=1)
    json.dump(summary, sys.stdout)
    print()


if __name__ == "__main__":
    main()
