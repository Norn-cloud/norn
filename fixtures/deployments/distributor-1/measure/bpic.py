#!/usr/bin/env python3
"""Fetch BPI Challenge 2019 (public, SAP purchase-to-pay, ~1.6M events) and
stream it into recorded exports for the d1.bpic mapping.

The XES is resolved through the 4TU.ResearchData API, then the figshare
mirror; the file never enters the repository. Stream-parsed with iterparse.

Usage: bpic.py fetch DIR | bpic.py convert XES OUT_DIR [--max-events N]
"""
from __future__ import annotations

import gzip
import json
import sys
import time
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

UA = {"User-Agent": "norn-deployment-test/0.1"}


def get_json(url: str):
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=60) as r:
        return json.loads(r.read())


def candidates() -> list[str]:
    urls = []
    for api in ("https://data.4tu.nl/v2/articles?search_for=BPI%20Challenge%202019&limit=20",):
        try:
            for art in get_json(api):
                if "2019" in art.get("title", "") and "OCEL" not in art.get("title", "") and "graph" not in art.get("title", "").lower():
                    for f in get_json(f"https://data.4tu.nl/v2/articles/{art['uuid']}/files"):
                        if f.get("name", "").lower().endswith((".xes", ".xes.gz")):
                            urls.append(f["download_url"])
        except Exception as e:
            print(f"4tu: {e}", file=sys.stderr)
    try:
        for f in get_json("https://api.figshare.com/v2/articles/12715853/files"):
            if f.get("name", "").lower().endswith((".xes", ".xes.gz", ".zip")):
                urls.append(f["download_url"])
    except Exception as e:
        print(f"figshare: {e}", file=sys.stderr)
    return urls


def fetch(dest: Path) -> Path:
    """The repository's pinned fetch first (scripts/bench_bpi.py: URL, SHA256 and licence check), then a search."""
    dest.mkdir(parents=True, exist_ok=True)
    try:
        import importlib.util
        spec = importlib.util.spec_from_file_location("bench_bpi", Path(__file__).resolve().parents[4] / "scripts/bench_bpi.py")
        bench = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bench)
        target = dest / "BPI_Challenge_2019.xes"
        bench.fetch(target)
        print(json.dumps({"pinned": bench.URL, "sha256": bench.SHA256, "bytes": target.stat().st_size}))
        return target
    except Exception as e:
        print(f"pinned fetch: {e}", file=sys.stderr)
    for url in candidates():
        name = url.rstrip("/").split("/")[-1].split("?")[0] or "bpic2019.xes"
        out = dest / (name if "." in name else "bpic2019.xes")
        try:
            t = time.time()
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=600) as r, open(out, "wb") as f:
                while chunk := r.read(1 << 20):
                    f.write(chunk)
            print(json.dumps({"url": url, "bytes": out.stat().st_size, "seconds": round(time.time() - t, 1)}))
            if out.suffix == ".zip":
                import zipfile
                with zipfile.ZipFile(out) as z:
                    inner = next(n for n in z.namelist() if n.lower().endswith((".xes", ".xes.gz")))
                    z.extract(inner, dest)
                    return dest / inner
            return out
        except Exception as e:
            print(f"download {url}: {e}", file=sys.stderr)
    raise SystemExit("BPIC 2019 not downloadable")


def attrs(el) -> dict:
    return {c.get("key"): c.get("value") for c in el if c.get("key")}


def convert(xes: Path, out: Path, max_events: int | None) -> dict:
    out.mkdir(parents=True, exist_ok=True)
    opener = gzip.open if xes.suffix == ".gz" else open
    vendors, n_cases, n_events, part = {}, 0, 0, 0
    buf: list[dict] = []
    t = time.time()

    def flush():
        nonlocal buf, part
        if buf:
            (out / f"bpic-{part:04d}.json").write_text(json.dumps({"profile": "d1-recorded/1", "source": "sap-p2p", "mapping": "d1.bpic.mapping", "connectorInstance": "distributor-1", "snapshot": buf}))
            part += 1
            buf = []

    with opener(xes, "rb") as f:
        for _, el in ET.iterparse(f, events=("end",)):
            if not el.tag.endswith("trace"):
                continue
            ta = attrs(el)
            cid = ta.get("concept:name")
            vendor = ta.get("Vendor") or "unknown"
            if vendor not in vendors:
                vendors[vendor] = ta.get("Name") or vendor
                buf.append({"dataset": "vendor", "record": {"id": vendor, "name": vendors[vendor]}})
            buf.append({"dataset": "case", "record": {"id": cid, "vendor": vendor, "company": ta.get("Company", ""), "doc_type": ta.get("Document Type", ""), "item_type": ta.get("Item Type", ""),
                                                      "item_category": ta.get("Item Category", ""), "spend_area": ta.get("Spend area text", ""), "sub_spend_area": ta.get("Sub spend area text", ""),
                                                      "spend_class": ta.get("Spend classification text", ""), "source": ta.get("Source", ""), "gr_based": ta.get("GR-Based Inv. Verif.") == "true",
                                                      "goods_receipt": ta.get("Goods Receipt") == "true", "purch_doc": ta.get("Purch. Doc.", ""), "item": ta.get("Item", "")}})
            n_cases += 1
            for k, ev in enumerate(c for c in el if c.tag.endswith("event")):
                a = attrs(ev)
                buf.append({"dataset": "event", "record": {"id": f"{cid}#{k}", "case": cid, "activity": a.get("concept:name", ""), "at": a.get("time:timestamp", ""),
                                                           "user": a.get("User", ""), "org_resource": a.get("org:resource", ""), "net_worth": str(a.get("Cumulative net worth (EUR)", "0"))}})
                n_events += 1
            el.clear()
            if len(buf) >= 20000:
                flush()
            if max_events and n_events >= max_events:
                break
    flush()
    return {"cases": n_cases, "events": n_events, "vendors": len(vendors), "parts": part, "seconds": round(time.time() - t, 1)}


if __name__ == "__main__":
    if sys.argv[1] == "fetch":
        print(fetch(Path(sys.argv[2])))
    else:
        mx = int(sys.argv[sys.argv.index("--max-events") + 1]) if "--max-events" in sys.argv else None
        print(json.dumps(convert(Path(sys.argv[2]), Path(sys.argv[3]), mx)))
