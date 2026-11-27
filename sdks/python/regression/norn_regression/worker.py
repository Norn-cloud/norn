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
import json
import sys
import time
import traceback
from pathlib import Path
from typing import Any

from .receipt import canonical_json_str, evaluate_forecast_receipt

FIELDS = ("request", "object_type", "target", "features")


def present(value) -> str | None:
    """A stored value, or None when it is absent (null, NA, or NaN after a dataframe read)."""
    import pandas as pd

    if value is None:
        return None
    if not isinstance(value, str) and bool(pd.isna(value)):
        return None
    return str(value)


def report(cell: Any, request: dict, status: str, spec: str | None = None, error: str | None = None) -> None:
    values = {field: str(request[field]) for field in FIELDS} | {
        "order_field": present(request.get("order_field")), "status": status, "spec": spec, "error": error[:2000] if error else None}
    cell.call("stats.checkrequest.finish", values, f"{request['request']}-{status}")


def run(cell: Any, request: dict) -> str:
    from .cell import check_regression

    report(cell, request, "running")
    try:
        features = [name.strip() for name in str(request["features"]).split(",") if name.strip()]
        result = check_regression(str(request["object_type"]), str(request["target"]), features, cell=cell,
                                  order=present(request.get("order_field")), request=str(request["request"]))
    except Exception as error:  # the reason goes to the request, where Studio shows it
        report(cell, request, "failed", error=f"{type(error).__name__}: {error}")
        return "failed"
    report(cell, request, "done", spec=result.spec["spec"])
    return "done"


def queued(cell: Any) -> list[dict]:
    rows = cell.rows("stats.CheckRequest")
    if rows.empty:
        return []
    return rows[rows["status"] == "queued"].sort_values("request").to_dict("records")


def evaluate_dispatched_receipt(invocation_path: Path, dataset_path: Path) -> str:
    """Execute one dispatched `ml.forecast.evaluate` invocation and return canonical receipt JSON."""
    import pandas as pd

    invocation = json.loads(invocation_path.read_text(encoding="utf-8"))
    dataset_rows = json.loads(dataset_path.read_text(encoding="utf-8"))
    if not isinstance(dataset_rows, list):
        raise ValueError("dataset payload must be a JSON array of row objects")
    frame = pd.DataFrame(dataset_rows)
    inputs = invocation["inputs"]
    features = [name for name in str(inputs.get("features", "")).split(",") if name]
    _, receipt, _, _ = evaluate_forecast_receipt(
        frame,
        str(inputs["target"]),
        features,
        tenant=str(invocation["tenant"]),
        authority_id=str(invocation["authority"]["id"]),
        command_id=str(invocation["commandId"]),
        version_id=str(inputs["version_id"]),
        model_id=str(inputs["model_id"]),
        evaluation_run=str(inputs["evaluation_run"]),
        dataset_revision=str(inputs["dataset_revision"]),
        metric_name=str(inputs["metric_name"]),
        order=inputs.get("order"),
        dispatched_invocation=invocation,
    )
    return canonical_json_str(receipt)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--project", help="use a local `norn dev` session instead of the environment")
    parser.add_argument("--once", action="store_true", help="run what is queued now, then exit")
    parser.add_argument("--interval", type=float, default=5.0, help="seconds between polls (Studio allows 120 requests a minute per key)")
    parser.add_argument("--evaluate-receipt", action="store_true", help="evaluate one dispatched ml.forecast.evaluate invocation and print canonical receipt JSON")
    parser.add_argument("--invocation", type=Path, help="path to dispatched FunctionInvocation JSON (with --evaluate-receipt)")
    parser.add_argument("--dataset", type=Path, help="path to dataset snapshot rows JSON (with --evaluate-receipt)")
    args = parser.parse_args()
    if args.evaluate_receipt:
        if args.invocation is None or args.dataset is None:
            parser.error("--evaluate-receipt requires --invocation and --dataset")
        print(evaluate_dispatched_receipt(args.invocation, args.dataset), flush=True)
        return 0
    from .cell import Cell

    with (Cell.from_project(args.project) if args.project else Cell.from_env()) as cell:
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
