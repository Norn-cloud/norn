#!/usr/bin/env python3
"""Fetch licensed Olist/UCI data at test time; emit link-free extracts and a SQLite oracle."""
import argparse
import csv
from datetime import datetime, timedelta, timezone
from decimal import Decimal, ROUND_HALF_EVEN
import hashlib
import json
from pathlib import Path
import sqlite3
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

OLIST_URL = "https://www.kaggle.com/api/v1/datasets/download/olistbr/brazilian-ecommerce?datasetVersionNumber=2"
OLIST_SHA = "967e41e04fc306fe604e2a693f488995a8b41e5047418f8a5c8e4abd6deca784"
UCI_URL = "https://archive.ics.uci.edu/static/public/502/online%2Bretail%2Bii.zip"
UCI_SHA = "572e36277c2390fbfde10664750731e0a86f55e33470d91919085f0408e67bfb"
SHEET_NS = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
BUDGET = 200_000_000


def fetch(url, sha256, path):
    if not path.exists():
        request = urllib.request.Request(url, headers={"User-Agent": "norn-bench"})
        with urllib.request.urlopen(request, timeout=120) as response, path.open("xb") as out:
            while chunk := response.read(1 << 20):
                if out.tell() + len(chunk) > BUDGET:
                    raise ValueError("download exceeds pinned size budget")
                out.write(chunk)
    if hashlib.sha256(path.read_bytes()).hexdigest() != sha256:
        path.unlink()
        raise ValueError(f"checksum mismatch for {url}")


def write_jsonl(path, rows):
    with path.open("w", encoding="utf-8") as out:
        count = 0
        for row in rows:
            out.write(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n")
            count += 1
    return count


def olist_rows(archive, name):
    with zipfile.ZipFile(archive) as bundle, bundle.open(name) as raw:
        yield from csv.DictReader(line.decode("utf-8") for line in raw)


def sheet_rows(bundle, member, strings):
    columns = "ABCDEFGH"
    with bundle.open(member) as stream:
        for _, element in ET.iterparse(stream):
            if element.tag != SHEET_NS + "row":
                continue
            row = dict.fromkeys(columns)
            for cell in element:
                value = cell.find(SHEET_NS + "v")
                if value is not None:
                    text = strings[int(value.text)] if cell.get("t") == "s" else value.text
                    row[cell.get("r").rstrip("0123456789")] = text
            yield row
            element.clear()


def uci_rows(archive):
    with zipfile.ZipFile(archive) as outer:
        workbook = next(n for n in outer.namelist() if n.endswith(".xlsx"))
        extracted = archive.with_suffix(".xlsx")
        extracted.write_bytes(outer.read(workbook))
    with zipfile.ZipFile(extracted) as bundle:
        strings = []
        with bundle.open("xl/sharedStrings.xml") as stream:
            for _, element in ET.iterparse(stream):
                if element.tag == SHEET_NS + "si":
                    strings.append("".join(t.text or "" for t in element.iter(SHEET_NS + "t")))
                    element.clear()
        for sheet, member in enumerate(sorted(n for n in bundle.namelist() if n.startswith("xl/worksheets/sheet"))):
            for index, row in enumerate(sheet_rows(bundle, member, strings)):
                if index:
                    yield sheet, row


def load_uci(database, archive):
    database.execute("""CREATE TABLE uci(sheet INT, invoice TEXT, code TEXT, description TEXT, quantity INT,
        at TEXT, price TEXT, customer TEXT, country TEXT)""")
    database.executemany("INSERT INTO uci VALUES (?,?,?,?,?,?,?,?,?)", (
        (sheet, r["A"], r["B"], r["C"], int(float(r["D"])), r["E"], r["F"],
         str(int(float(r["G"]))) if r["G"] else None, r["H"]) for sheet, r in uci_rows(archive)))


def sample(database, work):
    """Deterministic stratified sample; the 3dp price rounding is the declared precision policy."""
    strata = {
        "negative_price": "CAST(price AS REAL) < 0",
        "cancellation": "invoice LIKE 'C%'",
        "negative_not_cancellation": "quantity < 0 AND invoice NOT LIKE 'C%'",
        "missing_customer": "customer IS NULL",
        "clean": "customer IS NOT NULL AND quantity > 0 AND CAST(price AS REAL) >= 0 AND invoice NOT LIKE 'C%'",
    }
    database.execute("DROP TABLE IF EXISTS sampled")
    database.execute("CREATE TABLE sampled(id INTEGER PRIMARY KEY)")
    for where in strata.values():
        database.execute(f"INSERT OR IGNORE INTO sampled SELECT rowid FROM uci WHERE {where} ORDER BY rowid LIMIT 300")
    rows = []
    for rowid, invoice, code, quantity, at, price, customer, country in database.execute("""
            SELECT u.rowid, invoice, code, quantity, at, price, customer, country FROM uci u
            JOIN sampled s ON s.id = u.rowid ORDER BY u.rowid"""):
        moment = datetime(1899, 12, 30, tzinfo=timezone.utc) + timedelta(days=float(at))
        rows.append({"id": f"line-{rowid}", "invoice": invoice, "code": code, "quantity": quantity,
                     "price": float(Decimal(price).quantize(Decimal("0.001"), ROUND_HALF_EVEN)),
                     "customer": customer or "", "country": country,
                     "occurred_at": moment.strftime("%Y-%m-%dT%H:%M:%SZ")})
    write_jsonl(work / "uci-lines-sample.jsonl", rows)
    by_country = {}
    clean_by_country = {}
    for row in rows:
        by_country[row["country"]] = by_country.get(row["country"], 0) + row["quantity"]
        if row["price"] >= 0:
            clean_by_country[row["country"]] = clean_by_country.get(row["country"], 0) + row["quantity"]
    return {
        "rows": len(rows),
        "expected_findings": {
            "retail_price_nonnegative": sum(r["price"] < 0 for r in rows),
            "retail_customer_present": sum(not r["customer"] for r in rows),
            "retail_sign": sum(r["quantity"] <= 0 and not r["invoice"].startswith("C") for r in rows),
        },
        "sum_quantity_by_country": by_country,
        "sum_quantity_by_country_unquarantined": clean_by_country,
    }


def extract(args):
    work = Path(args.work)
    olist, uci = work / "olist.zip", work / "uci.zip"
    fetch(OLIST_URL, OLIST_SHA, olist)
    fetch(UCI_URL, UCI_SHA, uci)
    database = sqlite3.connect(work / "oracle.sqlite")
    database.execute("DROP TABLE IF EXISTS uci")
    load_uci(database, uci)
    database.commit()
    one = lambda sql: database.execute(sql).fetchone()[0]
    products = list(olist_rows(olist, "olist_products_dataset.csv"))
    customers = list(olist_rows(olist, "olist_customers_dataset.csv"))
    sellers = list(olist_rows(olist, "olist_sellers_dataset.csv"))
    counts = {
        "olist-products": write_jsonl(work / "olist-products.jsonl", (
            {"product_id": r["product_id"], "label": r["product_category_name"]} for r in products)),
        "olist-customers": write_jsonl(work / "olist-customers.jsonl", (
            {"id": key, "kind": "person"} for key in sorted({r["customer_unique_id"] for r in customers}))),
        "olist-sellers": write_jsonl(work / "olist-sellers.jsonl", (
            {"id": r["seller_id"], "kind": "organization"} for r in sellers)),
        # Description choice is an explicit grain decision: most frequent, then lexical.
        "uci-items": write_jsonl(work / "uci-items.jsonl", (
            {"product_id": code, "label": label or ""} for code, label in database.execute("""
            SELECT code, (SELECT description FROM uci v WHERE v.code = c.code AND description IS NOT NULL
                          GROUP BY description ORDER BY COUNT(*) DESC, description LIMIT 1)
            FROM (SELECT DISTINCT code FROM uci) c ORDER BY code"""))),
        "uci-customers": write_jsonl(work / "uci-customers.jsonl", (
            {"id": key, "kind": "person"} for (key,) in database.execute(
                "SELECT DISTINCT customer FROM uci WHERE customer IS NOT NULL ORDER BY customer"))),
        "uci-lines": write_jsonl(work / "uci-lines.jsonl", (
            {"id": f"{sheet}-{rowid}", "invoice": invoice, "code": code, "quantity": quantity,
             "price": price, "customer": customer or "", "country": country}
            for rowid, sheet, invoice, code, quantity, price, customer, country in database.execute(
                "SELECT rowid, sheet, invoice, code, quantity, price, customer, country FROM uci ORDER BY rowid"))),
    }
    oracle = {
        "extract_rows": counts,
        "uci_sample": sample(database, work),
        "olist": {
            "products_missing_category": sum(not r["product_category_name"] for r in products),
            "customer_rows": len(customers),
            "distinct_customer_unique_id": len({r["customer_unique_id"] for r in customers}),
        },
        "uci": {
            "rows": one("SELECT COUNT(*) FROM uci"),
            "missing_customer": one("SELECT COUNT(*) FROM uci WHERE customer IS NULL"),
            "negative_quantity": one("SELECT COUNT(*) FROM uci WHERE quantity < 0"),
            "negative_price": one("SELECT COUNT(*) FROM uci WHERE CAST(price AS REAL) < 0"),
            "zero_price": one("SELECT COUNT(*) FROM uci WHERE CAST(price AS REAL) = 0"),
            "cancellation_prefix": one("SELECT COUNT(*) FROM uci WHERE invoice LIKE 'C%'"),
            "negative_quantity_not_cancellation": one("SELECT COUNT(*) FROM uci WHERE quantity < 0 AND invoice NOT LIKE 'C%'"),
            "cancellation_not_negative": one("SELECT COUNT(*) FROM uci WHERE quantity >= 0 AND invoice LIKE 'C%'"),
            "price_more_than_3dp": one("SELECT COUNT(*) FROM uci WHERE LENGTH(price) - INSTR(price, '.') > 3 AND INSTR(price, '.') > 0"),
            "items_without_description": one("SELECT COUNT(*) FROM (SELECT code FROM uci GROUP BY code HAVING COUNT(description) = 0)"),
            "items_with_multiple_descriptions": one("SELECT COUNT(*) FROM (SELECT code FROM uci GROUP BY code HAVING COUNT(DISTINCT description) > 1)"),
            "non_alnum5_codes": one("SELECT COUNT(DISTINCT code) FROM uci WHERE code NOT GLOB '[0-9][0-9][0-9][0-9][0-9]*'"),
            "cross_sheet_duplicate_rows": one("""SELECT COUNT(*) FROM (SELECT 1 FROM uci GROUP BY invoice, code, description, quantity, at, price, customer, country
                HAVING COUNT(DISTINCT sheet) > 1)"""),
            "sum_quantity": one("SELECT SUM(quantity) FROM uci"),
            "sum_quantity_by_country_top": database.execute(
                "SELECT country, SUM(quantity) FROM uci GROUP BY country ORDER BY 2 DESC, 1 LIMIT 5").fetchall(),
        },
    }
    (work / "oracle.json").write_text(json.dumps(oracle, indent=1), encoding="utf-8")
    print(json.dumps(oracle))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("work", help="existing scratch directory outside the checkout")
    extract(parser.parse_args())


if __name__ == "__main__":
    main()
