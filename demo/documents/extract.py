"""Extract scanned documents into rows with per-field evidence, and serve them.

The rows have the `docs.scans.source` shape (registry/packs/documents): one
document record with its printed lines and one evidence row per extracted
value (document URI and SHA-256, page, box as page fractions, the text span
read, the extractor and its version, and the extractor's own confidence).
The cell pulls them through its `norn.pull/1` connector and maps them through
`docs.scans.mapping`, so lineage reaches the box. Nothing here writes to the
cell or decides anything about the values.

    extract.py files OUT.json IMAGE... [--uri-prefix P]    # local images -> rows
    extract.py seed --store DIR ROWS.json                  # add pre-extracted rows (the legacy import)
    extract.py run --store DIR                             # one shot: new objects under the bucket prefix
    extract.py serve --store DIR [--port 8080]             # POST /extractions for the cell's pull connector
    extract.py upload DIR [--prefix synthetic/scans/<seed>]  # put generated scans through the cell's upload route

`run` reads S3-compatible settings from the environment: NORN_SCANS_ENDPOINT
(https://<account>.r2.cloudflarestorage.com), NORN_SCANS_BUCKET,
NORN_SCANS_PREFIXES (comma-separated; default `synthetic/,uploads/`,
the prefixes the cell's upload route writes), and the read-only key id and secret from
NORN_SECRET_S3_SCANS_READ_KEY_ID / NORN_SECRET_S3_SCANS_READ_SECRET or the
files named by the same variables with a `_FILE` suffix. It never writes to
the bucket. A document already in the store is never extracted again, so a
re-run adds nothing and the legacy rows are never replaced by a re-extraction.

Extractors sit behind one interface (`Extractor.extract(image) -> Extraction`).
The baseline is Tesseract OCR (word boxes and confidences) with a layout
parser for the synthetic templates and a generic receipt fallback. A vision
model could be added behind the same interface; its boxes would have to be
grounded in OCR word boxes before they could be evidence.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import hmac
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PARSER_VERSION = "0.1.0"
AMOUNT = re.compile(r"^\$?(\d{1,7}(?:[.,]\d{1,2})?)$")
DATE = re.compile(r"(\d{4}-\d{2}-\d{2})")
NUMBER = re.compile(r"\b([A-Z]{2,4}-\d{3,6})\b")
LEDGER = re.compile(r"Ledger entry (JE-\d{3,6})")
MEDIA = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".pdf": "application/pdf"}


@dataclass
class Word:
    text: str
    box: tuple[int, int, int, int]  # left, top, width, height in pixels
    confidence: float


@dataclass
class Line:
    words: list[Word]

    @property
    def text(self) -> str:
        return " ".join(word.text for word in self.words)

    @property
    def last(self) -> Word:
        return self.words[-1]


@dataclass
class Value:
    value: str
    word: Word | None


@dataclass
class Extraction:
    kind: str
    doc_number: str | None
    vendor: str | None
    issued_on: str | None
    total: Value
    lines: list[tuple[str, Value]] = field(default_factory=list)
    width: int = 1
    height: int = 1
    ledger_ref: str | None = None


class Extractor:
    """One extraction provider: a name, a version, and image -> Extraction."""

    name = "abstract"
    version = "0"

    def extract(self, image: Path) -> Extraction | None:
        raise NotImplementedError


def amount(text: str) -> str | None:
    match = AMOUNT.match(text.strip())
    if not match:
        return None
    raw = match.group(1).replace(",", ".")
    try:
        Decimal(raw)
    except InvalidOperation:
        return None
    return raw


class Tesseract(Extractor):
    name = "tesseract"

    def __init__(self) -> None:
        banner = subprocess.run(["tesseract", "--version"], capture_output=True, text=True, check=True)
        engine = (banner.stdout or banner.stderr).split()[1]
        self.version = f"{engine}+norn-extractor/{PARSER_VERSION}"

    def lines(self, image: Path) -> tuple[list[Line], int, int]:
        """Words grouped into printed rows by their vertical centre, so a label
        and its amount far to the right stay on one row however the OCR
        segmented the page."""
        tsv = subprocess.run(["tesseract", str(image), "-", "--psm", "4", "tsv"], capture_output=True, text=True, check=True, timeout=120).stdout
        words: list[Word] = []
        width = height = 1
        for row in tsv.splitlines()[1:]:
            cells = row.split("\t")
            if len(cells) < 12:
                continue
            level, left, top, w, h, conf, text = cells[0], *map(int, cells[6:10]), float(cells[10]), cells[11]
            if level == "1":
                width, height = w, h
            if text.strip() and conf >= 0 and h > 0:
                words.append(Word(text.strip(), (left, top, w, h), conf))
        rows: list[list[Word]] = []
        for word in sorted(words, key=lambda word: word.box[1] + word.box[3] / 2):
            centre = word.box[1] + word.box[3] / 2
            if rows:
                last = rows[-1]
                band = sum(w.box[1] + w.box[3] / 2 for w in last) / len(last)
                tall = max(w.box[3] for w in last)
                if abs(centre - band) <= 0.6 * max(tall, word.box[3]):
                    last.append(word)
                    continue
            rows.append([word])
        return [Line(sorted(row, key=lambda word: word.box[0])) for row in rows], width, height

    def extract(self, image: Path) -> Extraction | None:
        lines, width, height = self.lines(image)
        texts = [line.text for line in lines]
        upper = [text.upper() for text in texts]
        kind = "invoice" if any(t.startswith("INVOICE") for t in upper) else "expense_report" if any("EXPENSE REPORT" in t for t in upper) else "receipt"
        number = next((m.group(1) for t in texts if (m := NUMBER.search(t))), None)
        issued = next((m.group(1) for t in texts if (m := DATE.search(t))), None)
        vendor = next((t.split(":", 1)[1].strip() for t in texts if t.lower().startswith(("from:", "purpose:"))), None)
        if vendor is None and lines:
            vendor = texts[0].title()
        total_at = next((i for i, t in enumerate(upper) if t.startswith("TOTAL") or t.startswith("AMOUNT CLAIMED")), None)
        if total_at is None:
            return None
        total_value = amount(lines[total_at].last.text)
        if total_value is None:
            return None
        printed: list[tuple[str, Value]] = []
        if kind != "expense_report":
            start = next((i + 1 for i, t in enumerate(upper) if t.startswith("DESCRIPTION")), None)
            if start is None:
                start = next((i + 1 for i, t in enumerate(texts) if DATE.search(t)), 0)
            for line in lines[start:total_at]:
                value = amount(line.last.text)
                if value is not None and len(line.words) > 1 and "SUBTOTAL" not in line.text.upper():
                    printed.append((" ".join(w.text for w in line.words[:-1]), Value(value, line.last)))
        ledger = next((m.group(1) for t in texts if (m := LEDGER.search(t))), None)
        return Extraction(kind, number, vendor, issued, Value(total_value, lines[total_at].last), printed, width, height,
                          f"demo/ledger/{ledger}" if ledger else None)


def evidence(doc: str, name: str, value: Value, uri: str, digest: str, media: str, extractor: Extractor, size: tuple[int, int]) -> dict:
    left, top, w, h = value.word.box if value.word else (0, 0, 0, 0)
    width, height = size
    pad = 3
    frac = lambda v, total: f"__D__{max(0.0, v / total):.4f}"
    return {"name": f"{doc}~{name}", "parent": doc, "field": name, "document_uri": uri, "document_digest": digest,
            "media_type": media, "page": 1, "bbox_x": frac(left - pad, width), "bbox_y": frac(top - pad, height),
            "bbox_w": frac(w + 2 * pad, width), "bbox_h": frac(h + 2 * pad, height), "text": value.word.text if value.word else value.value,
            "extractor": extractor.name, "extractor_version": extractor.version,
            "confidence_bp": round(value.word.confidence * 100) if value.word else None}


def row(doc: str, extraction: Extraction, uri: str, digest: str, media: str, extractor: Extractor) -> dict:
    size = (extraction.width, extraction.height)
    lines = [{"name": f"{doc}~line-{i}", "parent": doc, "line_no": i, "description": description, "amount": f"__D__{value.value}"}
             for i, (description, value) in enumerate(extraction.lines, 1)]
    evidences = [evidence(doc, "total", extraction.total, uri, digest, media, extractor, size)]
    evidences += [evidence(doc, f"lines.{i}.amount", value, uri, digest, media, extractor, size) for i, (_, value) in enumerate(extraction.lines, 1)]
    return {"name": doc, "doc_number": extraction.doc_number or doc.removeprefix("SCAN-"), "kind": extraction.kind,
            "vendor": extraction.vendor or "unknown", "issued_on": extraction.issued_on, "currency": "USD",
            "total": f"__D__{extraction.total.value}", "line_count": len(lines), "ledger_ref": extraction.ledger_ref,
            "document_uri": uri, "document_digest": digest, "media_type": media, "extractor": extractor.name,
            "extractor_version": extractor.version, "lines": lines, "evidence": evidences}


def dumps(value) -> str:
    return re.sub(r'"__D__(-?[0-9.]+)"', r"\1", json.dumps(value, sort_keys=True))


def document_id(key: str) -> str:
    stem = Path(key).stem
    stem = re.sub(r"[^A-Za-z0-9-]+", "-", stem).strip("-")[:80] or hashlib.sha256(key.encode()).hexdigest()[:12]
    return stem if stem.startswith("SCAN-") else f"SCAN-{stem}"


def page_image(data: bytes, media: str, scratch: Path) -> Path | None:
    if media == "application/pdf":
        try:
            import pypdfium2  # Apache-2.0 / BSD-3-Clause
        except ImportError:
            print("skip: PDF support needs pypdfium2", file=sys.stderr)
            return None
        page = pypdfium2.PdfDocument(data)[0]
        path = scratch / "page.png"
        page.render(scale=150 / 72).to_pil().save(path)
        return path
    path = scratch / ("page" + {"image/png": ".png", "image/jpeg": ".jpg"}[media])
    path.write_bytes(data)
    return path


class Store:
    """Append-only extracted rows, in (version, id) order; nothing is rewritten."""

    def __init__(self, directory: Path) -> None:
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=True)
        self.path = directory / "rows.jsonl"
        self.path.touch()

    def rows(self) -> list[dict]:
        return [json.loads(line) for line in self.path.read_text().splitlines() if line.strip()]

    def ids(self) -> set[str]:
        return {row["id"] for row in self.rows()}

    def append(self, records: list[dict]) -> int:
        known, version = self.ids(), len(self.rows())
        added = 0
        with self.path.open("a") as out:
            for record in records:
                if record["name"] in known:
                    continue
                version += 1
                known.add(record["name"])
                out.write(dumps({"id": record["name"], "version": f"{version:012d}", **record}) + "\n")
                added += 1
        return added


def secret(name: str) -> str:
    if os.environ.get(name):
        return os.environ[name]
    return Path(os.environ[f"{name}_FILE"]).read_text().strip()


class Bucket:
    """Read-only S3 ListObjectsV2 and GetObject with SigV4 (stdlib only)."""

    def __init__(self) -> None:
        self.endpoint = os.environ["NORN_SCANS_ENDPOINT"].rstrip("/")
        self.bucket = os.environ["NORN_SCANS_BUCKET"]
        self.key_id = secret("NORN_SECRET_S3_SCANS_READ_KEY_ID")
        self.secret = secret("NORN_SECRET_S3_SCANS_READ_SECRET")
        self.region = os.environ.get("NORN_SCANS_REGION", "auto")

    def request(self, path: str, query: dict[str, str]) -> bytes:
        now = dt.datetime.now(dt.timezone.utc)
        stamp, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
        host = urllib.parse.urlparse(self.endpoint).netloc
        canonical_path = urllib.parse.quote(f"/{self.bucket}/{path}".rstrip("/") if path else f"/{self.bucket}", safe="/-_.~")
        canonical_query = "&".join(f"{urllib.parse.quote(k, safe='-_.~')}={urllib.parse.quote(v, safe='-_.~')}" for k, v in sorted(query.items()))
        payload = hashlib.sha256(b"").hexdigest()
        headers = f"host:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{stamp}\n"
        signed = "host;x-amz-content-sha256;x-amz-date"
        request = "\n".join(["GET", canonical_path, canonical_query, headers, signed, payload])
        scope = f"{day}/{self.region}/s3/aws4_request"
        to_sign = "\n".join(["AWS4-HMAC-SHA256", stamp, scope, hashlib.sha256(request.encode()).hexdigest()])
        key = ("AWS4" + self.secret).encode()
        for part in (day, self.region, "s3", "aws4_request"):
            key = hmac.new(key, part.encode(), hashlib.sha256).digest()
        signature = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
        url = self.endpoint + canonical_path + ("?" + canonical_query if canonical_query else "")
        req = urllib.request.Request(url, headers={"x-amz-date": stamp, "x-amz-content-sha256": payload,
                                                   "Authorization": f"AWS4-HMAC-SHA256 Credential={self.key_id}/{scope}, SignedHeaders={signed}, Signature={signature}"})
        with urllib.request.urlopen(req, timeout=60) as response:
            return response.read()

    def keys(self, prefix: str) -> list[str]:
        keys, token = [], None
        while True:
            query = {"list-type": "2", "prefix": prefix}
            if token:
                query["continuation-token"] = token
            root = ET.fromstring(self.request("", query))
            ns = {"s3": root.tag.split("}")[0].strip("{")} if root.tag.startswith("{") else {}
            find = (lambda e, t: e.find(f"s3:{t}", ns)) if ns else (lambda e, t: e.find(t))
            for item in root.iter(f"{{{ns['s3']}}}Contents" if ns else "Contents"):
                keys.append(find(item, "Key").text)
            truncated = find(root, "IsTruncated")
            if truncated is None or truncated.text != "true":
                return sorted(keys)
            token = find(root, "NextContinuationToken").text

    def get(self, key: str) -> bytes:
        return self.request(key, {})


def extract_bytes(extractor: Extractor, key: str, data: bytes, uri: str) -> dict | None:
    media = MEDIA.get(Path(key).suffix.lower())
    if media is None:
        return None
    with tempfile.TemporaryDirectory() as scratch:
        image = page_image(data, media, Path(scratch))
        if image is None:
            return None
        extraction = extractor.extract(image)
    if extraction is None:
        print(f"skip: no printed total found in {key}", file=sys.stderr)
        return None
    return row(document_id(key), extraction, uri, hashlib.sha256(data).hexdigest(), media, extractor)


def command_files(args) -> None:
    extractor = Tesseract()
    records = []
    for image in args.images:
        path = Path(image)
        record = extract_bytes(extractor, path.name, path.read_bytes(), (args.uri_prefix or "file://") + path.name)
        if record:
            records.append(record)
    Path(args.out).write_text(dumps({"rows": records}) + "\n")
    print(json.dumps({"rows": len(records), "extractor": extractor.version}))


def command_seed(args) -> None:
    text = Path(args.rows).read_text()
    # Keep exact decimals: re-mark JSON numbers of decimal fields before loading.
    marked = re.sub(r'"(total|amount|bbox_[xywh])": (-?[0-9.]+)', r'"\1": "__D__\2"', text)
    added = Store(Path(args.store)).append(json.loads(marked)["rows"])
    print(json.dumps({"added": added}))


def command_run(args) -> None:
    store, bucket, extractor = Store(Path(args.store)), Bucket(), Tesseract()
    prefixes = os.environ.get("NORN_SCANS_PREFIXES", "synthetic/,uploads/").split(",")
    known, added = store.ids(), 0
    for key in [key for prefix in prefixes if prefix for key in bucket.keys(prefix)]:
        if document_id(key) in known or Path(key).suffix.lower() not in MEDIA:
            continue
        record = extract_bytes(extractor, key, bucket.get(key), f"s3://{bucket.bucket}/{key}")
        if record:
            added += store.append([record])
            known.add(record["name"])
    print(json.dumps({"added": added, "extractor": extractor.version}))


def command_upload(args) -> None:
    """begin -> PUT -> complete for each scan, through the cell's own upload
    route (the cell records the URI and the SHA-256 of the stored bytes). Reads
    NORN_URL (the host API base, e.g. https://alpha.<zone>/api/host), NORN_ORG,
    NORN_ENV and the API key from NORN_API_KEY or NORN_API_KEY_FILE."""
    base, key = os.environ["NORN_URL"].rstrip("/"), secret("NORN_API_KEY")
    scope = urllib.parse.urlencode({"orgId": os.environ["NORN_ORG"], "environmentId": os.environ["NORN_ENV"]})

    def call(method: str, path: str, body: dict | None = None) -> dict:
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(f"{base}{path}?{scope}", data=data, method=method,
                                         headers={"content-type": "application/json", "x-api-key": key})
        with urllib.request.urlopen(request, timeout=60) as response:
            return json.loads(response.read() or b"{}")

    done = []
    for path in sorted(Path(args.directory).glob("*")):
        media = MEDIA.get(path.suffix.lower())
        if media is None:
            continue
        data = path.read_bytes()
        begun = call("POST", "/v1/uploads:begin", {"fileName": path.name, "contentType": media, "size": len(data), "prefix": args.prefix})
        put = urllib.request.Request(begun["putUrl"], data=data, method="PUT", headers=begun.get("putHeaders", {}))
        urllib.request.urlopen(put, timeout=120).read()
        completed = call("POST", f"/v1/uploads/{begun['uploadId']}:complete", {})
        if completed.get("sha256") != hashlib.sha256(data).hexdigest():
            raise SystemExit(f"{path.name}: the cell recorded {completed.get('sha256')}, not the bytes sent")
        done.append(completed["uri"])
    print(json.dumps({"uploaded": len(done)}))


def command_serve(args) -> None:
    store = Store(Path(args.store))

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802 (http.server API)
            if self.path != "/extractions":
                self.send_error(404)
                return
            try:
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))) or b"{}")
                after = body.get("after") or ["", ""]
                limit = max(1, min(int(body.get("limit", 100)), 500))
            except (ValueError, TypeError):
                self.send_error(400)
                return
            position = (str(after[0]), str(after[1]))
            rows = [line for line in store.path.read_text().splitlines() if line.strip()]
            selected = [r for r in rows if (json.loads(r)["version"], json.loads(r)["id"]) > position][:limit]
            payload = ('{"rows":[' + ",".join(selected) + "]}").encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *_args) -> None:
            return

    ThreadingHTTPServer(("0.0.0.0", args.port), Handler).serve_forever()


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    files = sub.add_parser("files")
    files.add_argument("out")
    files.add_argument("images", nargs="+")
    files.add_argument("--uri-prefix")
    seed = sub.add_parser("seed")
    seed.add_argument("--store", required=True)
    seed.add_argument("rows")
    run = sub.add_parser("run")
    run.add_argument("--store", required=True)
    upload = sub.add_parser("upload")
    upload.add_argument("directory")
    upload.add_argument("--prefix", default="synthetic/scans/20261115")
    serve = sub.add_parser("serve")
    serve.add_argument("--store", required=True)
    serve.add_argument("--port", type=int, default=8080)
    args = parser.parse_args()
    {"files": command_files, "seed": command_seed, "run": command_run, "serve": command_serve, "upload": command_upload}[args.command](args)


if __name__ == "__main__":
    main()
