#!/usr/bin/env python3
"""Load recorded exports into a running `norn dev` through POST
/v1/observations:load, in pages the route admits (<=100 rows), and record
per-export rows, landed, already-landed, quarantined (with reasons), wall
time, rows/s and the host's resident memory.

Usage: load.py PROJECT_DIR OUT_JSON EXPORT.json [EXPORT.json ...] [--page 100] [--budget-s N]
"""
from __future__ import annotations

import json
import re
import sys
import time
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from norn import Norn, rss_kb  # noqa: E402


def pages(doc: dict, size: int):
    base = {k: v for k, v in doc.items() if k not in ("snapshot", "incremental")}
    snap, inc = doc.get("snapshot", []), doc.get("incremental", [])
    for i in range(0, len(snap), size):
        yield {**base, "snapshot": snap[i:i + size]}
    for i in range(0, len(inc), size):
        yield {**base, "snapshot": [], "incremental": inc[i:i + size]}


def load(n: Norn, doc: dict, size: int, budget: float | None) -> dict:
    res = {"rows": 0, "landed": 0, "alreadyLanded": 0, "quarantined": 0, "reasons": Counter(), "refusals": Counter(), "calls": 0, "seconds": 0.0, "latency": []}
    t0 = time.time()
    queue = list(pages(doc, size))
    while queue:
        page = queue.pop(0)
        if budget and time.time() - t0 > budget:
            res["stoppedByBudget"] = True
            break
        code, body, dt = n.call("POST", "/v1/observations:load", page)
        res["calls"] += 1
        res["latency"].append(dt)
        # The route caps a load at 100 rows AND 8 page commits; one row can fan out into several commits
        # (one per map it feeds). Split a refused page by the commits-per-row it reports and resend.
        if code == 413 and isinstance(body, dict):
            m = re.search(r"this one: (\d+) rows, (\d+) commits", body.get("message", ""))
            rows_in = len(page.get("snapshot") or page.get("incremental") or [])
            if m and rows_in > 1:
                per = max(1, int(int(m.group(1)) * 8 / max(int(m.group(2)), 1)))
                res["splits"] = res.get("splits", 0) + 1
                res["rowsPerCallAdmitted"] = per
                key = "snapshot" if page.get("snapshot") else "incremental"
                items = page[key]
                queue[:0] = [{**page, key: items[i:i + per]} for i in range(0, len(items), per)]
                continue
        if code != 200:
            res["refusals"][f"{code}:{(body or {}).get('code') or (body or {}).get('error', {}).get('code') if isinstance(body, dict) else code}"] += 1
            if len(res.setdefault("refusalSamples", [])) < 3:
                res["refusalSamples"].append(json.dumps(body)[:700])
            continue
        for k in ("rows", "landed", "alreadyLanded", "quarantined"):
            res[k] += int(body.get(k) or 0)
        for r in body.get("quarantineReasons") or []:
            res["reasons"][r if isinstance(r, str) else json.dumps(r, sort_keys=True)[:200]] += 1
    res["seconds"] = round(time.time() - t0, 2)
    lat = sorted(res.pop("latency"))
    res["callP50ms"] = round(1000 * lat[len(lat) // 2], 1) if lat else None
    res["callP95ms"] = round(1000 * lat[int(len(lat) * 0.95)], 1) if lat else None
    res["rowsPerSecond"] = round(res["rows"] / res["seconds"], 1) if res["seconds"] else None
    res["rssKb"] = rss_kb()
    res["reasons"] = dict(res["reasons"].most_common(25))
    res["refusals"] = dict(res["refusals"])
    return res


def main() -> None:
    args = sys.argv[1:]
    size = int(args[args.index("--page") + 1]) if "--page" in args else 100
    budget = float(args[args.index("--budget-s") + 1]) if "--budget-s" in args else None
    files = [a for a in args[2:] if a.endswith(".json")]
    n = Norn(Path(args[0]))
    out = Path(args[1])
    report = json.loads(out.read_text()) if out.exists() else {}
    for f in files:
        doc = json.loads(Path(f).read_text())
        r = load(n, doc, size, budget)
        report[Path(f).stem] = r
        print(json.dumps({Path(f).stem: {k: r[k] for k in ("rows", "landed", "quarantined", "seconds", "rowsPerSecond", "refusals")}}), flush=True)
        out.write_text(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
