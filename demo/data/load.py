"""Load recorded exports into a hosted cell through its admitted mappings.

    python3 demo/data/load.py BASE_URL ORG_ID ENV_ID FILE... < token

Reads a bearer token on stdin (so it stays out of the process list), then
POSTs each file made by `generate.py` (or any recorded export of that shape)
to `BASE_URL/v1/observations:load?orgId=...&environmentId=...` in slices, in
file order. Each slice is sized from the time per row measured on the slices
before it, to take about TARGET_SECONDS (3 s; NORN_LOAD_TARGET_SECONDS), and
stays within what one load may carry: 100 rows, 8 page commits (a row that
repeats a record already in its page starts the next commit, as a record's
history does) and 60 KiB of request body (the hosted Worker's bound is
64 KiB). A slice the cell still refuses as too large is halved and retried.
Each slice is captured and mapped by the cell in OBS-PAGE-1 pages; a row the
mapping refuses stays in the cell as quarantined evidence. Loading a file
again lands nothing new.

Prints one JSON line per file (rows, landed, already landed, quarantined
with reasons, and wall time) and a total line. Exits 1 if any slice is
refused or any row is quarantined.
"""

import json
import os
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

MAX_ROWS = 100
MAX_COMMITS = 8
MAX_BYTES = 60 * 1024
TARGET_SECONDS = float(os.environ.get("NORN_LOAD_TARGET_SECONDS", "3"))


class TooLarge(Exception):
    """The cell refused a slice as larger than one load may be."""


def post(url: str, token: str, body: dict) -> dict:
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        method="POST",
        headers={"authorization": f"Bearer {token}", "content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=600) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")[:500]
        if error.code == 413 and "observations.too_large" in detail:
            raise TooLarge(detail) from None
        sys.exit(f"{url}: {error.code} {detail}")


def identity(row: dict):
    record = row.get("record", {})
    return record.get("id", record.get("name"))


def take(header: dict, rows: list, start: int, limit: int) -> int:
    """How many rows from `start` the next slice carries: at most `limit`
    rows, MAX_COMMITS page commits and MAX_BYTES of body."""
    size = len(json.dumps({**header, "snapshot": []}))
    commits, dataset, seen, count = 0, None, set(), 0
    for row in rows[start : start + min(limit, MAX_ROWS)]:
        key = identity(row)
        if row.get("dataset") != dataset or key in seen:
            commits += 1
            dataset, seen = row.get("dataset"), set()
        seen.add(key)
        size += len(json.dumps(row)) + 2
        if count and (commits > MAX_COMMITS or size > MAX_BYTES):
            break
        count += 1
    return max(count, 1)


def main() -> None:
    if len(sys.argv) < 5:
        sys.exit(__doc__)
    base, org, env, files = sys.argv[1].rstrip("/"), sys.argv[2], sys.argv[3], sys.argv[4:]
    token = sys.stdin.read().strip()
    query = urllib.parse.urlencode({"orgId": org, "environmentId": env})
    url = f"{base}/v1/observations:load?{query}"
    total = {"rows": 0, "landed": 0, "alreadyLanded": 0, "quarantined": 0, "seconds": 0.0}
    failed = False
    for name in files:
        document = json.loads(Path(name).read_text())
        header = {key: value for key, value in document.items() if key != "snapshot"}
        rows = document["snapshot"]
        counts = {"rows": 0, "landed": 0, "alreadyLanded": 0, "quarantined": 0}
        reasons: dict = {}
        started = time.monotonic()
        start, limit = 0, 16
        while start < len(rows):
            count = take(header, rows, start, limit)
            begun = time.monotonic()
            try:
                answer = post(url, token, {**header, "snapshot": rows[start : start + count]})
            except TooLarge:
                if count == 1:
                    sys.exit(f"{name}: the cell refuses even a one-row load")
                limit = max(1, count // 2)
                continue
            spent = max(time.monotonic() - begun, 0.001)
            # Size the next slice from this one's time per row.
            limit = max(1, min(MAX_ROWS, int(count * TARGET_SECONDS / spent)))
            start += count
            for key in counts:
                counts[key] += answer[key]
            for code, n in answer.get("quarantineReasons", {}).items():
                reasons[code] = reasons.get(code, 0) + n
        seconds = round(time.monotonic() - started, 2)
        print(json.dumps({"file": name, **counts, "quarantineReasons": reasons, "seconds": seconds}))
        failed = failed or counts["quarantined"] > 0
        for key in counts:
            total[key] += counts[key]
        total["seconds"] = round(total["seconds"] + seconds, 2)
    print(json.dumps({"total": total}))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
