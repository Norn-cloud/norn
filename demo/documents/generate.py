"""Deterministic synthetic scanned documents for the provenance demo. No real data.

    python3 demo/documents/generate.py OUTDIR [--seed S] [--font PATH]
        [--uri-prefix s3://norn-alpha-documents/synthetic/scans/<seed>/]

Writes into OUTDIR:

- `scans/<id>.png`: invoices, receipts and expense reports rendered with
  seeded scan noise (a slight rotation, blur, grain, speckle and contrast).
  The same seed, font file and Pillow version give the same bytes.
- `truth.json`: what each document prints, with the page box of every value
  (fractions of the page, top-left origin), after the rotation.
- `legacy-ocr.rows.json`: the seeded "legacy OCR" import, as extracted rows in
  the `docs.scans.source` shape. It stands for an older system's output over
  the oldest documents. Two errors are seeded on purpose and documented in
  `manifest.json`: invoice INV-0042 prints 72.00 (lines 30.00 + 40.00 + 2.00)
  and the import reads 720; expense report RPT-0117 prints 72.0 and the import
  reads 720. Every other value is the printed one. Its confidences are
  synthetic.
- `manifest.json`: the seed, counts, the font file digest, the seeded errors
  and the SHA-256 of every file.

The item names are the synthetic catalog of `demo/data/generate.py`; vendors
are invented. Amounts are exact decimals written as JSON numbers.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import random
import re
from datetime import date, timedelta
from decimal import Decimal
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

SEED = 20261115
PAGE = (1240, 1754)  # A4 at 150 dpi
RECEIPT = (620, 1100)
ITEMS = ["Gold scrap", "Silver grain", "Copper cathode", "Watch case", "Clasp set", "Casting resin",
         "Casting flux", "Crucible liner", "Gift boxes", "Tissue paper", "Polishing cloth", "Ring sizer"]
VENDORS = ["Atlas Casting Supply", "Northgate Packaging", "Harbor Metals Exchange", "Quill & Loupe Tools",
           "Meridian Freight", "Cobalt Bench Works", "Lumen Display Co", "Saffron Courier"]
STORES = ["Corner Hardware", "Bench Cafe", "Metro Stationers", "Riverside Fuel"]
TRAVEL = ["Field travel", "Client visit", "Trade fair", "Assay lab visit"]
LEGACY_INVOICES = range(42, 46)  # INV-0042..INV-0045 came through the legacy OCR
REPORTS = range(101, 141)        # RPT-0101..RPT-0140, all through the legacy OCR
SEEDED_ERRORS = [
    {"document": "SCAN-INV-0042", "field": "total", "printed": "72.00", "imported": "720",
     "why": "dropped decimal point; the printed lines 30.00 + 40.00 + 2.00 contradict it (LAW A)"},
    {"document": "SCAN-RPT-0117", "field": "total", "printed": "72.0", "imported": "720",
     "why": "dropped decimal point on a report that prints no lines: no law applies; the profile flags it"},
]


def money(rng: random.Random, low: int, high: int) -> Decimal:
    return Decimal(rng.randint(low * 100, high * 100)) / 100


class Page:
    """One page being drawn, recording the box of every value it prints."""

    def __init__(self, size, font_path):
        self.image = Image.new("L", size, 255)
        self.draw = ImageDraw.Draw(self.image)
        self.font_path = font_path
        self.boxes: dict[str, list[float]] = {}

    def font(self, size):
        return ImageFont.truetype(self.font_path, size)

    def text(self, xy, text, size=28, key=None, anchor="la"):
        font = self.font(size)
        self.draw.text(xy, text, fill=0, font=font, anchor=anchor)
        if key:
            self.boxes[key] = list(self.draw.textbbox(xy, text, font=font, anchor=anchor))

    def rule(self, y, x0=80, x1=None):
        self.draw.line([(x0, y), (x1 or self.image.width - 80, y)], fill=0, width=2)


def scan(page: Page, rng: random.Random):
    """Seeded scan noise. Returns the image and the boxes as page fractions."""
    width, height = page.image.size
    angle = rng.uniform(-0.6, 0.6)
    image = page.image.rotate(angle, resample=Image.BICUBIC, fillcolor=246)
    theta = math.radians(angle)
    cx, cy = width / 2, height / 2

    def turn(x, y):
        # PIL rotates counter-clockwise on screen (y down).
        dx, dy = x - cx, y - cy
        return cx + dx * math.cos(theta) + dy * math.sin(theta), cy - dx * math.sin(theta) + dy * math.cos(theta)

    boxes = {}
    for key, (x0, y0, x1, y1) in page.boxes.items():
        corners = [turn(x, y) for x, y in ((x0, y0), (x1, y0), (x0, y1), (x1, y1))]
        xs, ys = [c[0] for c in corners], [c[1] for c in corners]
        pad = 3
        boxes[key] = [max(0.0, (min(xs) - pad) / width), max(0.0, (min(ys) - pad) / height),
                      (max(xs) - min(xs) + 2 * pad) / width, (max(ys) - min(ys) + 2 * pad) / height]
    image = image.filter(ImageFilter.GaussianBlur(radius=rng.uniform(0.3, 0.8)))
    pixels = image.load()
    for _ in range(width * height // 900):
        x, y = rng.randrange(width), rng.randrange(height)
        pixels[x, y] = rng.choice((0, 90, 200))
    lo = rng.randint(12, 30)
    image = image.point(lambda v: lo + v * (240 - lo) // 255)
    return image, boxes


def invoice(number: int, rng: random.Random, start: date) -> dict:
    lines = [(rng.choice(ITEMS), money(rng, 4, 60)) for _ in range(rng.randint(1, 4))]
    if number == 42:
        lines = [("Gift boxes", Decimal("30.00")), ("Tissue paper", Decimal("40.00")), ("Courier fee", Decimal("2.00"))]
    total = sum((amount for _, amount in lines), Decimal("0.00"))
    return {"id": f"SCAN-INV-{number:04d}", "doc_number": f"INV-{number:04d}", "kind": "invoice",
            "vendor": VENDORS[(number * 3) % len(VENDORS)], "issued_on": (start + timedelta(days=number * 3)).isoformat(),
            "lines": lines, "total": total, "total_text": f"{total:.2f}", "ledger_ref": f"demo/ledger/JE-{number:04d}"}


def receipt(number: int, rng: random.Random, start: date) -> dict:
    lines = [(rng.choice(["Coffee", "Sandpaper", "Notebook", "Fuel", "Tape", "Batteries"]), money(rng, 2, 40))
             for _ in range(rng.randint(1, 3))]
    total = sum((amount for _, amount in lines), Decimal("0.00"))
    return {"id": f"SCAN-RCP-{number:04d}", "doc_number": f"RCP-{number:04d}", "kind": "receipt",
            "vendor": STORES[number % len(STORES)], "issued_on": (start + timedelta(days=number)).isoformat(),
            "lines": lines, "total": total, "total_text": f"{total:.2f}", "ledger_ref": None}


def report(number: int, rng: random.Random, start: date) -> dict:
    amount = Decimal(rng.randint(150, 1500)) / 10
    if number == 117:
        amount = Decimal("72.0")
    return {"id": f"SCAN-RPT-{number:04d}", "doc_number": f"RPT-{number:04d}", "kind": "expense_report",
            "vendor": TRAVEL[number % len(TRAVEL)], "issued_on": (start + timedelta(days=number - 60)).isoformat(),
            "lines": [], "total": amount, "total_text": f"{amount:.1f}", "ledger_ref": None}


def render(doc: dict, font: str, rng: random.Random):
    if doc["kind"] == "receipt":
        page = Page(RECEIPT, font)
        page.text((310, 60), doc["vendor"].upper(), 30, "vendor", anchor="ma")
        page.text((310, 110), "RECEIPT", 26, "kind", anchor="ma")
        page.text((60, 170), f"No. {doc['doc_number']}", 22, "doc_number")
        page.text((60, 205), f"Date {doc['issued_on']}", 22, "issued_on")
        y = 270
        for index, (description, amount) in enumerate(doc["lines"], 1):
            page.text((60, y), description, 24)
            page.text((560, y), f"{amount:.2f}", 24, f"lines.{index}.amount", anchor="ra")
            y += 44
        page.rule(y + 6, 60, 560)
        page.text((60, y + 24), "TOTAL", 28)
        page.text((560, y + 24), doc["total_text"], 28, "total", anchor="ra")
    else:
        page = Page(PAGE, font)
        title = "INVOICE" if doc["kind"] == "invoice" else "EXPENSE REPORT"
        page.text((100, 100), title, 54, "kind")
        page.text((100, 200), f"From: {doc['vendor']}", 30, "vendor")
        page.text((100, 250), f"No. {doc['doc_number']}", 30, "doc_number")
        page.text((100, 300), f"Date {doc['issued_on']}", 30, "issued_on")
        if doc["kind"] == "invoice":
            page.text((100, 420), "Description", 28)
            page.text((1140, 420), "Amount (USD)", 28, anchor="ra")
            page.rule(462, 100, 1140)
            y = 490
            for index, (description, amount) in enumerate(doc["lines"], 1):
                page.text((100, y), description, 30)
                page.text((1140, y), f"{amount:.2f}", 30, f"lines.{index}.amount", anchor="ra")
                y += 56
            page.rule(y + 10, 100, 1140)
            page.text((700, y + 40), "TOTAL", 34)
            page.text((1140, y + 40), doc["total_text"], 34, "total", anchor="ra")
            page.text((100, 1600), f"Ledger entry {doc['ledger_ref'].rsplit('/', 1)[1]}", 22)
        else:
            page.text((100, 420), f"Purpose: {doc['vendor']}", 30)
            page.text((100, 520), "Amount claimed (USD)", 34)
            page.text((1140, 520), doc["total_text"], 34, "total", anchor="ra")
            page.text((100, 640), "Approved by: line manager", 26)
    return scan(page, rng)


def number(value: Decimal | str) -> str:
    """A JSON number token, exactly as written (scale kept)."""
    return f"__D__{value}"


def rows(doc: dict, boxes: dict, uri: str, digest: str, extractor: str, version: str, rng: random.Random, total: str) -> dict:
    """One extracted row in the `docs.scans.source` shape."""
    def evidence(field, text):
        x, y, w, h = boxes[field]
        return {"name": f"{doc['id']}~{field}", "parent": doc["id"], "field": field, "document_uri": uri,
                "document_digest": digest, "media_type": "image/png", "page": 1,
                "bbox_x": number(f"{x:.4f}"), "bbox_y": number(f"{y:.4f}"), "bbox_w": number(f"{w:.4f}"), "bbox_h": number(f"{h:.4f}"),
                "text": text, "extractor": extractor, "extractor_version": version, "confidence_bp": rng.randint(8600, 9800)}
    lines = [{"name": f"{doc['id']}~line-{index}", "parent": doc["id"], "line_no": index, "description": description,
              "amount": number(f"{amount:.2f}")} for index, (description, amount) in enumerate(doc["lines"], 1)]
    evidences = [evidence("total", total)] + [evidence(f"lines.{index}.amount", f"{amount:.2f}") for index, (_, amount) in enumerate(doc["lines"], 1)]
    return {"name": doc["id"], "doc_number": doc["doc_number"], "kind": doc["kind"], "vendor": doc["vendor"],
            "issued_on": doc["issued_on"], "currency": "USD", "total": number(total), "line_count": len(doc["lines"]),
            "ledger_ref": doc["ledger_ref"], "document_uri": uri, "document_digest": digest, "media_type": "image/png",
            "extractor": extractor, "extractor_version": version, "lines": lines, "evidence": evidences}


def dumps(value) -> str:
    return re.sub(r'"__D__(-?[0-9.]+)"', r"\1", json.dumps(value, indent=1, sort_keys=True)) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("outdir")
    parser.add_argument("--seed", type=int, default=SEED)
    parser.add_argument("--font", default="/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
    parser.add_argument("--uri-prefix", default=None)
    args = parser.parse_args()
    out = Path(args.outdir)
    (out / "scans").mkdir(parents=True, exist_ok=True)
    prefix = args.uri_prefix or f"s3://norn-alpha-documents/synthetic/scans/{args.seed}/"
    rng = random.Random(args.seed)
    start = date(2026, 1, 5)
    docs = [invoice(n, rng, start) for n in range(1, 61)] + [receipt(n, rng, start) for n in range(201, 241)] + [report(n, rng, start) for n in REPORTS]
    truth, legacy, digests = [], [], {}
    for doc in docs:
        image, boxes = render(doc, args.font, random.Random(f"{args.seed}/{doc['id']}"))
        path = out / "scans" / f"{doc['id']}.png"
        image.save(path, format="PNG", optimize=True)
        data = path.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        digests[f"scans/{path.name}"] = digest
        uri = prefix + path.name
        truth.append({"id": doc["id"], "kind": doc["kind"], "vendor": doc["vendor"], "doc_number": doc["doc_number"],
                      "issued_on": doc["issued_on"], "total": number(doc["total_text"]), "ledger_ref": doc["ledger_ref"],
                      "lines": [{"description": d, "amount": number(f"{a:.2f}")} for d, a in doc["lines"]],
                      "boxes": {k: [round(v, 4) for v in b] for k, b in sorted(boxes.items())}, "uri": uri, "sha256": digest})
        number_id = int(doc["doc_number"].split("-")[1])
        if (doc["kind"] == "invoice" and number_id in LEGACY_INVOICES) or doc["kind"] == "expense_report":
            seeded = next((e for e in SEEDED_ERRORS if e["document"] == doc["id"]), None)
            total = seeded["imported"] if seeded else doc["total_text"]
            legacy.append(rows(doc, boxes, uri, digest, "legacy-ocr", "4.2 (synthetic export)",
                               random.Random(f"{args.seed}/legacy/{doc['id']}"), total))
    files = {"truth.json": dumps(truth), "legacy-ocr.rows.json": dumps({"rows": legacy})}
    for name, text in files.items():
        (out / name).write_text(text)
        digests[name] = hashlib.sha256(text.encode()).hexdigest()
    manifest = {"seed": args.seed, "documents": len(docs), "legacyRows": len(legacy),
                "kinds": {kind: sum(d["kind"] == kind for d in docs) for kind in ("invoice", "receipt", "expense_report")},
                "font": {"path": args.font, "sha256": hashlib.sha256(Path(args.font).read_bytes()).hexdigest()},
                "uriPrefix": prefix, "seededErrors": SEEDED_ERRORS, "sha256": digests}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    print(json.dumps({k: manifest[k] for k in ("seed", "documents", "legacyRows", "kinds")}))


if __name__ == "__main__":
    main()
