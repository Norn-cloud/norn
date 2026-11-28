"""Parsers for distributor-1's file sources, stdlib only.

MT940 (SWIFT), ISO 20022 camt.053, EDI X12 850/856, xlsx with merged cells
(read with zipfile + ElementTree), and the Windows-1256 fixed-width aging
file. Each returns (dataset, record) rows for the d1.files mapping; 856 ASNs
are parsed and counted but have no mapping (see the gap report).
"""
from __future__ import annotations

import re
import xml.etree.ElementTree as ET
import zipfile
from decimal import Decimal
from pathlib import Path

PARSED: dict[str, int] = {}


def _n(key: str, k: int = 1) -> None:
    PARSED[key] = PARSED.get(key, 0) + k


INVOICE_RE = re.compile(r"(?:/INV/|INV )(SINV-\d{6})")


def mt940(text: str) -> list[dict]:
    rows, cur = [], None
    for line in re.split(r"\r?\n", text):
        if line.startswith(":61:"):
            m = re.match(r":61:(\d{6})(\d{4})?([CD])R?([\d,]+)N(\w{3})([^/]*)//(.*)", line)
            if not m:
                _n("mt940.unparsed_61")
                continue
            d = m.group(1)
            cur = {"id": f"A-{m.group(6)}", "bank": "BANK-A", "format": "mt940", "booked_at": f"20{d[:2]}-{d[2:4]}-{d[4:6]}T00:00:00",
                   "currency": "EGP", "amount": m.group(4).replace(",", "."), "remittance": "", "sign": m.group(3)}
            rows.append(cur)
        elif line.startswith(":86:") and cur is not None:
            cur["remittance"] = line[4:]
    for r in rows:
        if r.pop("sign") != "C":
            _n("mt940.debits_skipped")
    _n("mt940.lines", len(rows))
    return rows


def camt053(text: str) -> list[dict]:
    ns = {"c": "urn:iso:std:iso:20022:tech:xsd:camt.053.001.08"}
    root = ET.fromstring(text)
    rows = []
    for e in root.iterfind(".//c:Ntry", ns):
        amt = e.find("c:Amt", ns)
        rows.append({"id": f"B-{e.findtext('c:AcctSvcrRef', namespaces=ns)}", "bank": "BANK-B", "format": "camt.053",
                     "booked_at": f"{e.findtext('c:BookgDt/c:Dt', namespaces=ns)}T00:00:00", "currency": amt.get("Ccy"), "amount": amt.text,
                     "remittance": e.findtext(".//c:RmtInf/c:Ustrd", default="", namespaces=ns),
                     "counterparty_text": e.findtext(".//c:RltdPties/c:Dbtr/c:Pty/c:Nm", default=None, namespaces=ns)})
    _n("camt053.entries", len(rows))
    return rows


def x12(text: str) -> list[list[str]]:
    return [s.strip().split("*") for s in text.split("~") if s.strip()]


def x12_850(path: Path, xw, careful: bool) -> list[tuple[str, dict]]:
    segs = x12(path.read_text())
    po = {s[0]: s for s in segs}
    num, date = po["BEG"][3], po["BEG"][5]
    head = {"id": num, "po_number": num, "buyer_ref": f"d1/party/{po['N1'][2]}" if po["N1"][2] not in xw.unmapped else None,
            "po_date": f"{date[:4]}-{date[4:6]}-{date[6:8]}T00:00:00" + ("+02:00" if careful else ""), "stated_total": po["AMT"][2], "currency": "EGP"}
    out = [("edi.850", head)]
    for s in segs:
        if s[0] == "PO1":
            qty, price = Decimal(s[2]), Decimal(s[4])
            out.append(("edi.850.line", {"id": f"{num}/{s[1]}", "po_id": num, "item_ref": xw.item.get(s[7]), "qty": str(qty), "price": str(price), "amount": str(qty * price), "currency": "EGP"}))
    ctt = int(po["CTT"][1])
    if ctt != len(out) - 1:
        _n("x12.850.ctt_mismatch")
    _n("x12.850.docs")
    return out


def x12_856(path: Path) -> None:
    segs = x12(path.read_text())
    if not any(s[0] == "BSN" for s in segs):
        _n("x12.856.unparsed")
    _n("x12.856.docs")


def xlsx_rows(path: Path) -> tuple[list[list[str | None]], list[str]]:
    ns = {"m": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    with zipfile.ZipFile(path) as z:
        strings = [si.findtext("m:t", default="", namespaces=ns) for si in ET.fromstring(z.read("xl/sharedStrings.xml")).iterfind("m:si", ns)]
        sheet = ET.fromstring(z.read("xl/worksheets/sheet1.xml"))
    grid = []
    for row in sheet.iterfind(".//m:sheetData/m:row", ns):
        cells: dict[int, str] = {}
        for c in row.iterfind("m:c", ns):
            col = ord(re.match(r"[A-Z]", c.get("r")).group(0)) - 65
            v = c.findtext("m:v", namespaces=ns)
            cells[col] = strings[int(v)] if c.get("t") == "s" and v is not None else v
        grid.append([cells.get(i) for i in range(max(cells) + 1 if cells else 0)])
    merges = [m.get("ref") for m in sheet.iterfind(".//m:mergeCells/m:mergeCell", ns)]
    return grid, merges


def pricelist(path: Path, xw, careful: bool) -> list[tuple[str, dict]]:
    grid, merges = xlsx_rows(path)
    _n("xlsx.merged_ranges", len(merges))
    # two header rows under merged cells: the column name is the merged group plus the sub header
    header = []
    for i in range(5):
        top = next((grid[1][j] for j in range(i, -1, -1) if j < len(grid[1]) and grid[1][j]), "")
        sub = grid[2][i] if i < len(grid[2]) and grid[2][i] else ""
        header.append(f"{top}/{sub}".strip("/"))
    out = []
    for r in grid[3:]:
        rec = dict(zip(header, r + [None] * (5 - len(r))))
        lp_text, wp_text = rec.get("Price/List EGP") or "", rec.get("Price/Web EGP") or ""
        lp = lp_text.replace("EGP", "").replace(",", "").strip()
        wp = wp_text.replace(",", ".").strip()
        out.append(("pricelist", {"id": rec.get("Item/Code"), "item_ref": xw.item.get(rec.get("Item/Code")), "code": rec.get("Item/Code"), "name_ar": rec.get("Item/Name (AR)"),
                                  "list_price": lp if careful else lp_text, "list_price_text": lp_text, "web_price": (wp or None) if careful else wp_text, "web_price_text": wp_text, "group": rec.get("Group")}))
    _n("xlsx.rows", len(out))
    return out


LAYOUT = [("cust_no", 1, 8), ("name_ar", 9, 48), ("region_ar", 49, 63), ("balance", 64, 77), ("last_txn", 78, 87), ("flag", 88, 88)]


def aging(path: Path, xw, careful: bool) -> list[tuple[str, dict]]:
    raw = path.read_bytes()
    text = raw.decode("cp1256") if careful else raw.decode("utf-8", errors="replace")  # naive: assumes UTF-8
    out = []
    for line in text.splitlines():
        if not line.strip():
            continue
        f = {k: line[a - 1:b].strip() for k, a, b in LAYOUT}
        d, m, y = f["last_txn"].split("/")
        out.append(("aging", {"id": f["cust_no"], "x_norn_ref": xw.party_by_legacy.get(f["cust_no"]), "name_ar": f["name_ar"], "region_ar": f["region_ar"],
                              "balance": f["balance"].replace(",", "") if careful else f["balance"], "last_txn": f"{y}-{m}-{d}" if careful else f["last_txn"], "blocked": f["flag"] == "X"}))
    _n("fixed_width.lines", len(out))
    return out


def all_records(files_dir: Path, xw, world: dict, careful: bool) -> list[tuple[str, dict]]:
    xw.unmapped = set(world["unmapped_customers"])
    out: list[tuple[str, dict]] = []
    receipts = mt940((files_dir / "bank-a.mt940").read_text()) + camt053((files_dir / "bank-b.camt053.xml").read_text())
    for r in receipts:
        m = INVOICE_RE.search(r["remittance"] or "")
        r["invoice_hint"] = m.group(1) if m else None  # candidate only: reconciliation is a governed action
        r.setdefault("counterparty_text", None)
        if careful:
            r["booked_at"] += "+02:00"
        out.append(("bank.receipt", r))
    for p in sorted((files_dir / "edi").glob("*.850.x12")):
        out += x12_850(p, xw, careful)
    for p in sorted((files_dir / "edi").glob("*.856.x12")):
        x12_856(p)
    out += pricelist(files_dir / "pricelist.xlsx", xw, careful)
    out += aging(files_dir / "aging-legacy.cp1256.txt", xw, careful)
    return out
