"""Load orders.json into a running `norn dev` cell through the governed action route.

    python3 demo/data/load_orders.py PROJECT_DIR ORDERS_JSON [--limit N]

Each row is one `sales.order.add` call with an idempotency key derived from
its order reference, so a rerun replays instead of duplicating. Decimals
travel as exact decimal text. Prints rows, wall time and ms per row.
"""

import argparse
import http.client
import json
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("project")
parser.add_argument("orders")
parser.add_argument("--limit", type=int)
args = parser.parse_args()
project = Path(args.project)
session = json.loads((project / ".norn/dev.json").read_text())
token = (project / ".norn/dev.token").read_text().strip()
host, port = session["url"].removeprefix("http://").split(":")
scope = session["scope"]
rows = json.loads(Path(args.orders).read_text())["rows"][: args.limit]
fields = ["order_ref", "order_date", "ship_date", "lead_time_days", "customer_ref", "segment", "region",
          "item_code", "quantity", "unit", "unit_price", "currency", "system"]
started = time.perf_counter()
outcomes = {}
for row in rows:
    body = json.dumps({"input": {field: row[field] for field in fields}, "idempotencyKey": f"order:{row['order_ref']}"})
    connection = http.client.HTTPConnection(host, int(port), timeout=60)
    connection.request("POST", f"/v1/actions/sales.order.add?orgId={scope['orgId']}&environmentId={scope['environmentId']}",
                       body, {"authorization": f"Bearer {token}", "content-type": "application/json"})
    response = connection.getresponse()
    document = json.loads(response.read())
    if response.status != 200:
        raise SystemExit(f"{row['order_ref']} refused ({response.status}): {document}")
    outcomes[document["outcome"]] = outcomes.get(document["outcome"], 0) + 1
elapsed = time.perf_counter() - started
print(json.dumps({"rows": len(rows), "outcomes": outcomes, "seconds": round(elapsed, 1),
                  "msPerRow": round(elapsed * 1000 / max(len(rows), 1), 1)}))
