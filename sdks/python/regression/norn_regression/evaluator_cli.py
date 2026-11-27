"""Hermetic CLI entrypoint for `ml.forecast.evaluate` v1 (`Dockerfile.evaluator`).

    python -m norn_regression.evaluator_cli \
        --invocation invocation.json \
        --snapshot snapshot.norn1.json \
        --model model.norn1.json

Reads the exact dispatched `FunctionInvocation`, raw `norn.snapshot/v1` dataset
snapshot bytes, and raw `norn.linear-model/v1` model artifact bytes without any
type-coercing intermediate loader, verifies every digest and input pin, scores
the registered model artifact on the hashed snapshot, and prints the canonical
`FunctionReceipt` JSON to stdout.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from .receipt import canonical_json_str, evaluate_forecast_receipt


def run_cli(invocation_path: Path, snapshot_path: Path, model_path: Path) -> str:
    """Execute one dispatched evaluation from raw pinned bytes and return canonical receipt JSON."""
    invocation = json.loads(invocation_path.read_text(encoding="utf-8"))
    snapshot_bytes = snapshot_path.read_bytes()
    model_bytes = model_path.read_bytes()
    inputs = invocation["inputs"]
    features_raw = str(inputs.get("features", ""))
    features = [name for name in features_raw.split(",") if name]
    _, receipt, _, _ = evaluate_forecast_receipt(
        snapshot_bytes,
        model_bytes,
        str(inputs["target"]),
        features,
        tenant=str(invocation["tenant"]),
        authority_id=str(invocation["authority"]["id"]),
        authority_kind=str(invocation["authority"]["kind"]),
        generation=invocation["run"]["generation"],
        command_id=str(invocation["commandId"]),
        version_id=str(inputs["version_id"]),
        model_id=str(inputs["model_id"]),
        evaluation_run=str(inputs["evaluation_run"]),
        dataset_revision=str(inputs["dataset_revision"]),
        metric_name=str(inputs["metric_name"]),
        order=inputs["order"],
        dispatched_invocation=invocation,
    )
    return canonical_json_str(receipt)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--invocation", type=Path, required=True, help="path to dispatched FunctionInvocation JSON")
    parser.add_argument("--snapshot", type=Path, required=True, help="path to raw norn.snapshot/v1 canonical JSON bytes")
    parser.add_argument("--model", type=Path, required=True, help="path to raw norn.linear-model/v1 model artifact bytes")
    args = parser.parse_args(argv)
    print(run_cli(args.invocation, args.snapshot, args.model), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
