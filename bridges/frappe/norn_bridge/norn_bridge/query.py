"""Frappe list arguments -> the host's object grid query (HOST-READ-3).

Translation only: filters, sort and paging go to the cell, which filters and
withholds by the caller's grants. Nothing is filtered, joined or counted here;
a Frappe argument the grid grammar cannot express is refused with its name.
"""

from __future__ import annotations

import json
import re

import frappe

from norn_bridge import bundle, client

PAGE = 200
MAX_SCAN = 2000

OPS = {
    "=": "eq",
    "!=": "ne",
    "in": "in",
    "<": "lt",
    "<=": "lte",
    ">": "gt",
    ">=": "gte",
    "between": "between",
}
_IDENT = re.compile(r"[`\"]?(?:tab[^`\"]+[`\"]?\.)?[`\"]?([A-Za-z_][A-Za-z0-9_]*)[`\"]?")
_AGG = re.compile(r"^\s*(count|sum|avg|min|max)\s*\(\s*([^)]*)\)\s*(?:as\s+[`\"]?(\w+)[`\"]?)?\s*$", re.I)


class Unsupported(frappe.ValidationError):
    pass


def refuse(what: str):
    raise Unsupported(f"Norn bridge: {what} is not served by the cell's grid query yet")


def _field(text: str) -> str:
    match = _IDENT.fullmatch(text.strip())
    if not match:
        refuse(f"the field expression {text!r}")
    return match.group(1)


def _triples(filters) -> list[tuple[str, str, object]]:
    if not filters:
        return []
    if isinstance(filters, str):
        filters = json.loads(filters)
    if isinstance(filters, dict):
        items = []
        for name, value in filters.items():
            if isinstance(value, (list, tuple)) and len(value) == 2 and isinstance(value[0], str):
                items.append((name, value[0], value[1]))
            else:
                items.append((name, "=", value))
        return items
    out = []
    for item in filters:
        if hasattr(item, "fieldname") and hasattr(item, "operator"):
            out.append((item.fieldname, item.operator, item.value))
        elif isinstance(item, (list, tuple)) and len(item) in (2, 3, 4):
            parts = list(item)
            if len(parts) == 4:
                parts = parts[1:]
            if len(parts) == 2:
                parts = [parts[0], "=", parts[1]]
            out.append(tuple(parts))
        else:
            refuse(f"the filter {item!r}")
    return out


def _clause(entry: dict, name: str, op: str, value) -> dict:
    prop = entry["fields"].get(name)
    if name == "name":
        prop = entry.get("idProperty")
    if prop is None:
        refuse(f"filtering on {name!r}")
    op = op.lower().strip()
    if op == "like" and isinstance(value, str):
        core = value.strip("%")
        if value.startswith("%") and value.endswith("%"):
            return {"property": prop, "op": "contains", "value": core}
        if value.endswith("%") and not value.startswith("%"):
            return {"property": prop, "op": "prefix", "value": core}
        return {"property": prop, "op": "eq", "value": core}
    if op == "is":
        return {"property": prop, "op": "present" if value == "set" else "empty"}
    if op not in OPS:
        refuse(f"the filter operator {op!r}")
    if op == "in" and isinstance(value, str):
        value = [part.strip() for part in value.split(",") if part.strip()]
    return {"property": prop, "op": OPS[op], "value": value}


def _sort(entry: dict, order_by: str | None) -> str | None:
    keys = []
    for part in (order_by or "").split(","):
        part = part.strip()
        if not part:
            continue
        expression, _, direction = part.rpartition(" ")
        if not expression:
            expression, direction = part, "asc"
        prop = entry["fields"].get(_field(expression))
        # Frappe's default sort is `modified`; records carry a version, not a
        # time, so an unknown sort key falls back to the cell's id order.
        if prop:
            keys.append(f"{prop}:{direction.lower()}")
    return ",".join(keys) or None


def to_row(doctype: str, entry: dict, record: dict) -> dict:
    values = record.get("values") or {}
    version = record.get("version") or 0
    stamp = f"1970-01-01 00:00:00.{version:06d}" if version < 10**6 else None
    row = {
        "doctype": doctype,
        "name": record["id"],
        "docstatus": 0,
        "modified": stamp,
        "creation": stamp,
        "owner": "Administrator",
        "modified_by": "Administrator",
        "_norn_version": version,
        "_norn_withheld": bool(record.get("withheld")),
    }
    for fieldname, prop in entry["fields"].items():
        row[fieldname] = _plain(values.get(prop))
    return row


def _plain(value):
    if isinstance(value, dict) and set(value) <= {"decimal", "scale", "value", "kind"}:
        return value.get("value") or value.get("decimal")
    return value


def _page(doctype, entry, clauses, sort, after, limit):
    query = {"limit": str(limit), "after": after, "sort": sort}
    if clauses:
        query["filter"] = json.dumps({"all": clauses}, separators=(",", ":"))
    try:
        return client.query_objects(entry["objectType"], **query)
    except client.Refused as refused:
        frappe.throw(refused.document.get("message") or str(refused), title="Norn")


def get_list(kwargs: dict):
    args = frappe._dict(kwargs.get("args") or kwargs)
    doctype = args.doctype
    entry = bundle.object_type(doctype)
    if args.get("or_filters"):
        refuse("or_filters")
    clauses = [_clause(entry, *t) for t in _triples(args.get("filters"))]
    fields = args.get("fields") or ["name"]
    if isinstance(fields, str):
        fields = json.loads(fields) if fields.startswith("[") else [fields]
    aggregates = [f for f in fields if isinstance(f, dict) or _AGG.match(str(f))]
    if args.get("group_by"):
        return _grouped(doctype, entry, clauses, fields, aggregates, _field(args.group_by))
    if aggregates:
        return _aggregate(doctype, entry, clauses, aggregates)
    start = int(args.get("start") or args.get("limit_start") or 0)
    length = int(args.get("page_length") or args.get("limit_page_length") or 20) or MAX_SCAN
    sort = _sort(entry, args.get("order_by"))
    rows, after = [], None
    while len(rows) < start + length and len(rows) < MAX_SCAN:
        page = _page(doctype, entry, clauses, sort, after, PAGE)
        rows += [to_row(doctype, entry, r) for r in page.get("records", [])]
        after = page.get("next")
        if not after:
            break
    rows = rows[start : start + length]
    names = [_field(str(f)) for f in fields]
    if "*" in fields:
        names = list(rows[0]) if rows else []
    if args.get("pluck"):
        return [row.get(args.pluck) for row in rows]
    shaped = [{n: row.get(n) for n in names} for row in rows]
    if args.get("as_list"):
        return [list(row.values()) for row in shaped]
    return shaped


def _aggregate(doctype, entry, clauses, aggregates):
    """COUNT is the cell's total; other aggregates need a Norn question."""
    out = {}
    for item in aggregates:
        if isinstance(item, dict):
            function = next(k for k in item if k != "as")
            alias = item.get("as", "result")
        else:
            match = _AGG.match(str(item))
            function, alias = match.group(1), match.group(3) or "count"
        if function.lower() != "count":
            refuse(f"the aggregate {function.upper()} (publish it as a Norn question)")
        out[alias] = count(doctype, entry, clauses)
    return [out]


def _grouped(doctype, entry, clauses, fields, aggregates, group):
    """GROUP BY a declared enum: one cell count per value, in declared order."""
    values = entry.get("enums", {}).get(group)
    plain = [_field(str(f)) for f in fields if f not in aggregates]
    if values is None or plain not in ([], [group]):
        refuse(f"grouping by {group!r}")
    rows = []
    for value in values:
        row = {group: value}
        for item in _aggregate(doctype, entry, [*clauses, _clause(entry, group, "=", value)], aggregates):
            row.update(item)
        if any(v for k, v in row.items() if k != group):
            rows.append(row)
    if frappe._dict(fields=fields).get("as_list"):
        return [list(r.values()) for r in rows]
    return rows


def count(doctype, entry, clauses) -> int:
    page = _page(doctype, entry, clauses, None, None, 1)
    total = page.get("total") or {}
    if "count" not in total:
        refuse("a total count for this query")
    return int(total["count"])


def get_count(kwargs: dict) -> int:
    args = frappe._dict(kwargs.get("args") or kwargs)
    entry = bundle.object_type(args.doctype)
    clauses = [_clause(entry, *t) for t in _triples(args.get("filters"))]
    return count(args.doctype, entry, clauses)


def get_stats(kwargs: dict) -> dict:
    """Sidebar group-by counts: one cell count per declared enum value."""
    args = frappe._dict(kwargs.get("args") or kwargs)
    # Frappe's sidebar passes only stats and filters; the DocType is the request's.
    doctype = args.doctype or kwargs.get("doctype") or frappe.form_dict.get("doctype")
    entry = bundle.object_type(doctype)
    stats = args.get("stats") or []
    if isinstance(stats, str):
        stats = json.loads(stats)
    base = [_clause(entry, *t) for t in _triples(args.get("filters"))]
    out = {}
    for fieldname in stats:
        values = entry.get("enums", {}).get(fieldname)
        if not values:
            continue
        out[fieldname] = [
            [value, count(doctype, entry, [*base, _clause(entry, fieldname, "=", value)])]
            for value in values
        ]
    return out
