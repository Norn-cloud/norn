#!/usr/bin/env python3
"""Measure release Rust binary and wasm32 artifact sizes and emit METRIC lines.

Expects artifacts to have been built during the probe's `prepare` step:
  - <target_dir>/release/norn
  - <rivet_target_dir>/release/norn-cell-rivet
  - <target_dir>/wasm32-unknown-unknown/wasm-release/norn_checker_wasm.wasm
  - <target_dir>/wasm32-unknown-unknown/wasm-release/norn_cell_do.wasm

Gzip sizes use deterministic Python gzip (compresslevel=9, mtime=0). Standard library only.
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import sys
from pathlib import Path


def gzip_size(data: bytes) -> int:
    return len(gzip.compress(data, compresslevel=9, mtime=0))


def _read_non_empty(path: Path) -> bytes:
    if not path.is_file() or path.is_symlink():
        raise RuntimeError(f"missing built artifact: {path}")
    data = path.read_bytes()
    if not data:
        raise RuntimeError(f"built artifact is empty: {path}")
    return data


def measure_artifacts(target_dir: Path, rivet_target_dir: Path) -> list[dict]:
    norn_cli = target_dir / "release" / "norn"
    rivet_bin = rivet_target_dir / "release" / "norn-cell-rivet"
    checker_wasm = target_dir / "wasm32-unknown-unknown" / "wasm-release" / "norn_checker_wasm.wasm"
    cell_do_wasm = target_dir / "wasm32-unknown-unknown" / "wasm-release" / "norn_cell_do.wasm"

    cli_bytes = len(_read_non_empty(norn_cli))
    rivet_bytes = len(_read_non_empty(rivet_bin))
    checker_data = _read_non_empty(checker_wasm)
    cell_do_data = _read_non_empty(cell_do_wasm)

    return [
        {
            "metric": "binary.norn_cli.bytes",
            "value": cli_bytes,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"crate": "norn-cli", "artifact": "norn", "profile": "release"},
        },
        {
            "metric": "binary.norn_cell_rivet.bytes",
            "value": rivet_bytes,
            "unit": "bytes",
            "statistic": "single",
            "workload": {"crate": "norn-cell-rivet", "artifact": "norn-cell-rivet", "profile": "release"},
        },
        {
            "metric": "binary.norn_checker_wasm.bytes",
            "value": len(checker_data),
            "unit": "bytes",
            "statistic": "single",
            "workload": {"crate": "norn-checker-wasm", "target": "wasm32-unknown-unknown", "profile": "wasm-release"},
        },
        {
            "metric": "binary.norn_checker_wasm.gzip_bytes",
            "value": gzip_size(checker_data),
            "unit": "bytes",
            "statistic": "single",
            "workload": {
                "crate": "norn-checker-wasm",
                "target": "wasm32-unknown-unknown",
                "profile": "wasm-release",
                "gzip_level": 9,
            },
        },
        {
            "metric": "binary.norn_cell_do_wasm.bytes",
            "value": len(cell_do_data),
            "unit": "bytes",
            "statistic": "single",
            "workload": {"crate": "norn-cell-do", "target": "wasm32-unknown-unknown", "profile": "wasm-release"},
        },
        {
            "metric": "binary.norn_cell_do_wasm.gzip_bytes",
            "value": gzip_size(cell_do_data),
            "unit": "bytes",
            "statistic": "single",
            "workload": {
                "crate": "norn-cell-do",
                "target": "wasm32-unknown-unknown",
                "profile": "wasm-release",
                "gzip_level": 9,
            },
        },
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--target-dir",
        default=os.environ.get("CARGO_TARGET_DIR") or "target",
        help="Cargo target directory for root workspace artifacts (default: $CARGO_TARGET_DIR or target)",
    )
    parser.add_argument(
        "--rivet-target-dir",
        help="Cargo target directory for crates/norn-cell-rivet (default: <target-dir>/rivet)",
    )
    args = parser.parse_args(argv)
    target_dir = Path(args.target_dir).resolve()
    rivet_target_dir = (
        Path(args.rivet_target_dir).resolve() if args.rivet_target_dir else target_dir / "rivet"
    )

    for item in measure_artifacts(target_dir, rivet_target_dir):
        print(f"METRIC {json.dumps(item, sort_keys=True)}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
