"""Load recorded exports into a hosted cell through its admitted mappings.

    python3 demo/data/load.py BASE_URL ORG_ID ENV_ID FILE... < token

Reads a bearer token on stdin (so it stays out of the process list), then
POSTs each file made by `generate.py` (or any recorded export of that shape)
to `BASE_URL/v1/observations:load?orgId=...&environmentId=...` in slices, in
file order: at most 500 snapshot rows and 60 KiB of request body (the hosted
Worker accepts request bodies up to 64 KiB). Each slice is captured and mapped
by the cell in OBS-PAGE-1 pages; a row the mapping refuses stays in the
cell as quarantined evidence. Loading a file again lands nothing new.

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

# NORN_LOAD_SLICE_ROWS lowers the rows per request (default 500), for a host
# whose per-request time bound a slice of slow rows would exceed.
SLICE_ROWS = int(os.environ.get("NORN_LOAD_SLICE_ROWS", "500"))
SLICE_BYTES = 60 * 1024


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
        sys.exit(f"{url}: {error.code} {error.read().decode(errors='replace')[:500]}")


def slices(header: dict, rows: list) -> list:
    """Consecutive rows, at most SLICE_ROWS and SLICE_BYTES of body each."""
    out, current, size = [], [], len(json.dumps({**header, "snapshot": []}))
    base = size
    for row in rows:
        width = len(json.dumps(row)) + 2
        if current and (len(current) >= SLICE_ROWS or size + width > SLICE_BYTES):
            out.append(current)
            current, size = [], base
        current.append(row)
        size += width
    if current:
        out.append(current)
    return out


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
        for page in slices(header, rows):
            answer = post(url, token, {**header, "snapshot": page})
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
