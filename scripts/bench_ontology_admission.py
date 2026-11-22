#!/usr/bin/env python3
"""Measure checker admission at ontology scale against today's canon (#1512, #1417).

Each set is admitted in its own release-mode process (`crates/norn-formats/tests/owl_scale.rs`),
so the reported peak resident set is that set's. Input is an OWL import written outside the
repository by `norn-format-read <manifest> --originals <dir> --emit-dir <dir> --align <table>`;
nothing upstream is committed. Run on a build host (Boat), never on the shared agent host:

    python3 scripts/bench_ontology_admission.py --emit-dir <dir> [--permille 125,250,500,1000] [--repeat 3]

Prints one JSON object: every run's record (`set`, `artifacts`, `extends`, `links`,
`properties`, `admitMillis`, `errors`, `peakKiB`; for `lending:<mode>`, the definitions
install and the per-commit latency of `crates/norn-cell/tests/ontology_lending.rs` with the
vocabulary installed and implemented, or not at all) and, for the OWL vocabulary, the fitted
exponent of admission time in the artifact count (time ~ n^k), which says whether admission
grows linearly or with the set squared (#1417 C1/C5).
"""

from __future__ import annotations

import argparse
import json
import math
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCALE = ["cargo", "test", "--release", "--locked", "-p", "norn-formats", "--test", "suites", "--",
         "--ignored", "--exact", "owl_scale::owl_scale_admission", "--nocapture", "--test-threads", "1"]
LENDING = ["cargo", "test", "--release", "--locked", "-p", "norn-cell", "--test", "suites", "--",
           "--ignored", "--exact", "ontology_lending::owl_lending_cell", "--nocapture", "--test-threads", "1"]


def run(emit_dir: Path, scale_set: str) -> dict:
    env = dict(os.environ, NORN_OWL_EMIT_DIR=str(emit_dir))
    if scale_set.startswith("lending:"):
        command, marker = LENDING, "NORN_OWL_LENDING "
        env["NORN_LENDING_MODE"] = scale_set.removeprefix("lending:")
    else:
        command, marker = SCALE, "NORN_OWL_SCALE "
        env["NORN_OWL_SCALE_SET"] = scale_set
    out = subprocess.run(command, cwd=ROOT, env=env, check=True, capture_output=True, text=True).stdout
    # libtest prints `test <name> ... ` on the same line before the record.
    line = next(line for line in out.splitlines() if marker in line)
    record = json.loads(line.split(marker, 1)[1])
    record.setdefault("set", scale_set)
    return record


def exponent(points: list[tuple[int, float]]) -> float | None:
    """Least-squares slope of log(time) on log(n)."""
    points = [(n, t) for n, t in points if n > 0 and t > 0]
    if len(points) < 2:
        return None
    xs = [math.log(n) for n, _ in points]
    ys = [math.log(t) for _, t in points]
    mx, my = sum(xs) / len(xs), sum(ys) / len(ys)
    var = sum((x - mx) ** 2 for x in xs)
    return None if var == 0 else round(sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / var, 2)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--emit-dir", type=Path, required=True)
    parser.add_argument("--permille", default="125,250,500,1000")
    parser.add_argument("--repeat", type=int, default=3)
    args = parser.parse_args()
    emit_dir = args.emit_dir.resolve()
    if ROOT in emit_dir.parents or emit_dir == ROOT:
        raise SystemExit("the import is upstream-derived: keep --emit-dir outside the repository")
    sets = ["three-systems", *(f"owl:{f}" for f in args.permille.split(","))]
    if (emit_dir / "packs.json").exists():
        sets.append("owl-packs")
    sets += ["lending:ontology", "lending:plain"]
    runs = []
    for scale_set in sets:
        records = [run(emit_dir, scale_set) for _ in range(args.repeat)]
        best = min(records, key=lambda record: record.get("admitMillis", 0))
        best["admitMillisRuns"] = [record.get("admitMillis") for record in records]
        if "commit" in best:
            best["commitP50UsRuns"] = [record["commit"]["p50_us"] for record in records]
        runs.append(best)
    owl = [(r["artifacts"], r["admitMillis"]) for r in runs if r["set"].startswith("owl:")]
    print(json.dumps({"runs": runs, "owlTimeExponent": exponent(owl), "cpus": os.cpu_count()}, indent=2))


if __name__ == "__main__":
    main()
