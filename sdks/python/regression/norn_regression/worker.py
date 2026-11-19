"""The server-side job runner: one click in Studio queues a request; this runs it.

    python -m norn_regression.worker [--once] [--interval 5]

Reads its cell from the environment (`Cell.from_env`: NORN_URL, ORG, ENV,
NORN_TOKEN_FILE), or from a project's `norn dev` session with `--project DIR`.
It polls `stats.CheckRequest` for queued requests, and for each one reports
`running`, runs `check_regression` (which records the stats.ModelSpec and its
stats.AssumptionCheck rows through their governed actions), then reports
`done` with the spec id, or `failed` with the reason. Everything it does goes
through governed actions, so each step has a receipt. Stateless: the queue is
the cell. One worker per cell (there is no claim between workers).
"""

from __future__ import annotations

import argparse
import sys
import time
import traceback

from .cell import Cell, check_regression

FIELDS = ("request", "object_type", "target", "features")


def present(value) -> str | None:
    """A stored value, or None when it is absent (null, or NaN after a dataframe read)."""
    return None if value is None or (isinstance(value, float) and value != value) else str(value)


def report(cell: Cell, request: dict, status: str, spec: str | None = None, error: str | None = None) -> None:
    values = {field: str(request[field]) for field in FIELDS} | {
        "order_field": present(request.get("order_field")), "status": status, "spec": spec, "error": error[:2000] if error else None}
    cell.call("stats.checkrequest.finish", values, f"{request['request']}-{status}")


def run(cell: Cell, request: dict) -> str:
    report(cell, request, "running")
    try:
        features = [name for name in str(request["features"]).split(",") if name]
        result = check_regression(str(request["object_type"]), str(request["target"]), features, cell=cell,
                                  order=present(request.get("order_field")), request=str(request["request"]))
    except Exception as error:  # the reason goes to the request, where Studio shows it
        report(cell, request, "failed", error=f"{type(error).__name__}: {error}")
        return "failed"
    report(cell, request, "done", spec=result.spec["spec"])
    return "done"


def queued(cell: Cell) -> list[dict]:
    rows = cell.rows("stats.CheckRequest")
    if rows.empty:
        return []
    return rows[rows["status"] == "queued"].sort_values("request").to_dict("records")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--project", help="use a local `norn dev` session instead of the environment")
    parser.add_argument("--once", action="store_true", help="run what is queued now, then exit")
    parser.add_argument("--interval", type=float, default=5.0, help="seconds between polls (Studio allows 120 requests a minute per key)")
    args = parser.parse_args()
    cell = Cell.from_project(args.project) if args.project else Cell.from_env()
    while True:
        try:
            for request in queued(cell):
                print(f"{request['request']}: {run(cell, request)}", flush=True)
        except Exception:
            traceback.print_exc()
            if args.once:
                return 1
        if args.once:
            return 0
        time.sleep(args.interval)


if __name__ == "__main__":
    sys.exit(main())
