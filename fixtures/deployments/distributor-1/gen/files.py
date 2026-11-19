#!/usr/bin/env python3
"""Render the file-based sources of distributor-1 from world.json.

* BANK-A: SWIFT MT940 statement (receipts; references in :86: free text)
* BANK-B: ISO 20022 camt.053.001.08 statement
* EDI X12 850 (a customer's purchase order to us) and 856 (a supplier's ASN)
* An Excel (xlsx) customer price-list export with merged header cells and
  money as text, written by hand with zipfile (no third-party writer)
* A fixed-width, Windows-1256 encoded Arabic customer aging file from the
  pre-ERP system, with timezone-less dates and right-aligned text amounts
* legacy.sql: the legacy warehouse database (text quantities, local times)

Stdlib only. Usage: files.py WORLD_JSON OUT_DIR
"""
from __future__ import annotations

import json
import sys
import zipfile
from decimal import Decimal
from pathlib import Path
from xml.sax.saxutils import escape


def mt940(world: dict, pays: list[dict]) -> str:
    cust = {c["id"]: c for c in world["customers"]}
    out = [":20:STMT-BANKA-0001", ":25:EG380019000500000000263180002", ":28C:00001/001"]
    bal = Decimal("1000000.00")
    out.append(f":60F:C250101EGP{str(bal).replace('.', ',')}")
    for p in pays:
        d = p["at"][2:10].replace("-", "")
        amt = Decimal(p["amount"])
        bal += amt
        ref = {"invoice": f"/INV/{p['invoice']}", "free_text": f"PAYMENT FROM {cust[p['customer']]['name_en'].upper()[:20]}", "none": "TRF"}[p["ref_style"] if p["invoice"] else "none"]
        out.append(f":61:{d}{d[2:]}C{str(amt).replace('.', ',')}NTRF{p['id']}//{p['id'][-6:]}")
        out.append(f":86:{ref}")
    out.append(f":62F:C{pays[-1]['at'][2:10].replace('-', '') if pays else '250101'}EGP{str(bal).replace('.', ',')}")
    return "{1:F01BANKAEGCAXXX0000000000}{2:I940DISTEGCAXXXXN}{4:\r\n" + "\r\n".join(out) + "\r\n-}"


def camt053(world: dict, pays: list[dict]) -> str:
    cust = {c["id"]: c for c in world["customers"]}
    entries = []
    for p in pays:
        ustrd = f"INV {p['invoice']}" if p["invoice"] and p["ref_style"] == "invoice" else "RECEIPT"
        entries.append(f"""   <Ntry><Amt Ccy="{p['currency']}">{p['amount']}</Amt><CdtDbtInd>CRDT</CdtDbtInd><Sts><Cd>BOOK</Cd></Sts>
    <BookgDt><Dt>{p['at'][:10]}</Dt></BookgDt><ValDt><Dt>{p['at'][:10]}</Dt></ValDt><AcctSvcrRef>{p['id']}</AcctSvcrRef>
    <NtryDtls><TxDtls><Refs><EndToEndId>{p['id']}</EndToEndId></Refs><RltdPties><Dbtr><Pty><Nm>{escape(cust[p['customer']]['name_en'])}</Nm></Pty></Dbtr></RltdPties>
    <RmtInf><Ustrd>{escape(ustrd)}</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>""")
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.08">
 <BkToCstmrStmt><GrpHdr><MsgId>BANKB-STMT-0001</MsgId><CreDtTm>2026-11-01T06:00:00</CreDtTm></GrpHdr>
  <Stmt><Id>BANKB-0001</Id><Acct><Id><IBAN>EG800002000156789012345180002</IBAN></Id><Ccy>EGP</Ccy></Acct>
{chr(10).join(entries)}
  </Stmt></BkToCstmrStmt></Document>
"""


def x12_850(world: dict, inv: dict) -> str:
    items = {i["id"]: i for i in world["items"]}
    seg = ["ISA*00*          *00*          *ZZ*BUYER1         *ZZ*DISTRIBUTOR1   *251103*1200*U*00401*000000101*0*P*>",
           "GS*PO*BUYER1*DISTRIBUTOR1*20251103*1200*101*X*004010", "ST*850*0001",
           f"BEG*00*SA*{inv['order']}**{inv['at'].replace('-', '')}", f"N1*BY*{inv['customer']}"]
    for l in inv["lines"]:
        seg.append(f"PO1*{l['line']}*{l['qty']}*EA*{l['rate']}**VP*{items[l['item']]['erpnext_code']}")
    seg += [f"CTT*{len(inv['lines'])}", f"AMT*TT*{inv['stated_total']}", f"SE*{len(inv['lines']) + 6}*0001", "GE*1*101", "IEA*1*000000101"]
    return "~\n".join(seg) + "~\n"


def x12_856(world: dict, rec: dict) -> str:
    items = {i["id"]: i for i in world["items"]}
    seg = ["ISA*00*          *00*          *ZZ*SUPPLIER1      *ZZ*DISTRIBUTOR1   *251104*0800*U*00401*000000202*0*P*>",
           "GS*SH*SUPPLIER1*DISTRIBUTOR1*20251104*0800*202*X*004010", "ST*856*0001",
           f"BSN*00*{rec['id']}*{rec['at'][:10].replace('-', '')}*0800", "HL*1**S", f"N1*ST*DISTRIBUTOR1*92*{rec['wh']}",
           "HL*2*1*O", f"PRF*{rec['po']}", "HL*3*2*I", f"LIN**VP*{items[rec['item']]['erpnext_code']}", f"SN1**{rec['qty']}*EA",
           "CTT*1", "SE*12*0001", "GE*1*202", "IEA*1*000000202"]
    return "~\n".join(seg) + "~\n"


def xlsx(path: Path, world: dict) -> None:
    """Price list: row 1 is a merged title, rows 2-3 a two-level merged header, money as text."""
    rows = [["Distributor-1 price list (export)", None, None, None, None],
            ["Item", None, "Price", None, "Group"],
            ["Code", "Name (AR)", "List EGP", "Web EGP", None]]
    for it in world["items"]:
        rows.append([it["erpnext_code"], it["name_ar"], f"EGP {it['price']}", f"{it['web_price']}".replace(".", ","), it["group"]])
    merges = ["A1:E1", "A2:B2", "C2:D2", "E2:E3"]
    strings: list[str] = []
    sidx: dict[str, int] = {}

    def s(v: str) -> int:
        if v not in sidx:
            sidx[v] = len(strings)
            strings.append(v)
        return sidx[v]

    cols = "ABCDE"
    xr = []
    for r, row in enumerate(rows, 1):
        cells = "".join(f'<c r="{cols[c]}{r}" t="s"><v>{s(v)}</v></c>' for c, v in enumerate(row) if v is not None)
        xr.append(f'<row r="{r}">{cells}</row>')
    sheet = ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
             f'<sheetData>{"".join(xr)}</sheetData><mergeCells count="{len(merges)}">' + "".join(f'<mergeCell ref="{m}"/>' for m in merges) + "</mergeCells></worksheet>")
    sst = ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
           f'count="{len(strings)}" uniqueCount="{len(strings)}">' + "".join(f"<si><t>{escape(t)}</t></si>" for t in strings) + "</sst>")
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                   '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'
                   '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
                   '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
                   '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>')
        z.writestr("_rels/.rels", '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                   '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>')
        z.writestr("xl/workbook.xml", '<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
                   'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Prices" sheetId="1" r:id="rId1"/></sheets></workbook>')
        z.writestr("xl/_rels/workbook.xml.rels", '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                   '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
                   '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>')
        z.writestr("xl/worksheets/sheet1.xml", sheet)
        z.writestr("xl/sharedStrings.xml", sst)


# Fixed-width layout of the pre-ERP aging file (1-based columns, inclusive), written in Windows-1256.
FIXED_LAYOUT = [("cust_no", 1, 8), ("name_ar", 9, 48), ("region_ar", 49, 63), ("balance", 64, 77), ("last_txn", 78, 87), ("flag", 88, 88)]


def fixed_width(world: dict) -> bytes:
    lines = []
    for n, c in enumerate(world["customers"]):
        bal = f"{Decimal(c['credit_limit']) / 7:,.2f}"
        rec = (f"{c['id'].replace('C', 'K'):<8}{c['name_ar'][:40]:<40}{c['region_ar'][:15]:<15}{bal:>14}"
               f"{'%02d/%02d/2024' % (1 + n % 28, 1 + n % 12):<10}{'A' if n % 9 else 'X'}")
        lines.append(rec)
    return ("\r\n".join(lines) + "\r\n").encode("cp1256")


def legacy_sql(world: dict) -> str:
    items = {i["id"]: i for i in world["items"]}
    drift = {(d["item"], d["wh"]): d["qty"] for d in world["legacy_drift"]}
    out = ["CREATE SCHEMA IF NOT EXISTS wms;",
           "CREATE TABLE wms.itm_mst (itm_cd varchar(12) PRIMARY KEY, itm_desc_ar text, uom varchar(8), std_cost_txt varchar(20), actv char(1));",
           "CREATE TABLE wms.stk_bal (itm_cd varchar(12), whs_cd varchar(8), qty_txt varchar(16), upd_dt timestamp without time zone, PRIMARY KEY (itm_cd, whs_cd));",
           "CREATE TABLE wms.stk_txn (txn_id serial PRIMARY KEY, itm_cd varchar(12), whs_cd varchar(8), qty_txt varchar(16), txn_dt timestamp without time zone, ref_no varchar(20));"]
    for it in world["items"]:
        out.append(f"INSERT INTO wms.itm_mst VALUES ('{it['legacy_code']}', '{it['name_ar']}', '{it['uom'][:8]}', '{it['cost']}', 'Y');")
    for s in world["closing_stock"]:
        if s["wh"] not in ("WH-CAI", "WH-ALX"):
            continue
        q = drift.get((s["item"], s["wh"]), s["qty"])
        out.append(f"INSERT INTO wms.stk_bal VALUES ('{items[s['item']]['legacy_code']}', '{s['wh'][-3:]}', '{q:,}', '2026-10-31 23:00:00');")
    for m in world["moves"]:
        if m["wh"] in ("WH-CAI", "WH-ALX") and not m["ref"].endswith("7"):  # the WMS lost every movement whose ref ends in 7
            out.append(f"INSERT INTO wms.stk_txn (itm_cd, whs_cd, qty_txt, txn_dt, ref_no) VALUES ('{items[m['item']]['legacy_code']}', '{m['wh'][-3:]}', '{m['qty']}', '{m['at'].replace('T', ' ')}', '{m['ref']}');")
    return "\n".join(out) + "\n"


def main() -> None:
    world = json.loads(Path(sys.argv[1]).read_text())
    out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    a = sorted([p for p in world["payments"] if p["bank"] == "BANK-A"], key=lambda p: p["at"])
    b = sorted([p for p in world["payments"] if p["bank"] == "BANK-B"], key=lambda p: p["at"])
    (out / "bank-a.mt940").write_text(mt940(world, a), newline="")
    (out / "bank-b.camt053.xml").write_text(camt053(world, b))
    edi = out / "edi"
    edi.mkdir(exist_ok=True)
    for inv in [i for i in world["invoices"] if i["system"] == "erpnext"][:40]:
        (edi / f"{inv['order']}.850.x12").write_text(x12_850(world, inv))
    for rec in world["receipts"][:40]:
        (edi / f"{rec['id']}.856.x12").write_text(x12_856(world, rec))
    xlsx(out / "pricelist.xlsx", world)
    (out / "aging-legacy.cp1256.txt").write_bytes(fixed_width(world))
    (out / "legacy.sql").write_text(legacy_sql(world))
    print(json.dumps({p.name: p.stat().st_size for p in sorted(out.rglob("*")) if p.is_file() and p.parent == out}))


if __name__ == "__main__":
    main()
