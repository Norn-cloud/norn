#!/usr/bin/env python3
"""The KPI ratchet: fail when a tracked metric regresses beyond its noise band.

Runs nightly over the metrics branch, never on a pull request. Only series measured on
Boat count (`env.runner == "boat"`); GitHub-hosted numbers are too noisy to gate on.
`ratchet.json` lists the tracked metrics, which direction is better and the noise band.

For each tracked metric, its Boat values in series order (record.series_key: the measured
commit's date, then measurement time) give:

* baseline: the best rolling median of `window` consecutive values seen before the latest
  `confirm` values. It only moves in the better direction, so a slow drift still trips it;
  the median keeps one lucky run from setting an unreachable bar.
* current: the median of the latest `confirm` values.

A metric fails when current is worse than baseline by more than `noise_pct` percent. Too
little history passes with a note. Exit 1 on any failure.

    python3 scripts/metrics/ratchet.py --rev origin/metrics [--config scripts/metrics/ratchet.json]
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def judge(values: list[float], better: str, noise_pct: float, window: int = 3, confirm: int = 1) -> dict:
    """Verdict for one series (values in measurement order)."""
    if better not in ("lower", "higher"):
        raise ValueError(f"better must be lower or higher, not {better!r}")
    if len(values) < window + confirm:
        return {"status": "insufficient", "points": len(values), "needed": window + confirm}
    history, recent = values[:-confirm], values[-confirm:]
    medians = [statistics.median(history[i:i + window]) for i in range(len(history) - window + 1)]
    baseline = min(medians) if better == "lower" else max(medians)
    current = statistics.median(recent)
    if baseline == 0:
        change = 0.0 if current == 0 else float("inf")
    else:
        change = (current - baseline) / abs(baseline) * 100
    worse = change if better == "lower" else -change
    status = "regressed" if worse > noise_pct else "improved" if worse < -noise_pct else "within"
    return {"status": status, "baseline": baseline, "current": current, "change_pct": round(change, 2),
            "noise_pct": noise_pct, "points": len(values)}


def evaluate(records: list[dict], config: dict) -> list[dict]:
    sys.path.insert(0, str(HERE))
    import record as store  # noqa: PLC0415
    runner = config.get("runner", "boat")
    by_metric: dict[str, list[dict]] = {}
    for record in records:
        if record["env"]["runner"] == runner and not record["estimate"]:
            by_metric.setdefault(record["metric"], []).append(record)
    verdicts = []
    for metric, rule in sorted(config["metrics"].items()):
        rows = sorted(by_metric.get(metric, []), key=store.series_key)
        verdict = judge([r["value"] for r in rows], rule["better"], rule["noise_pct"],
                        rule.get("window", 3), rule.get("confirm", 1))
        verdict["metric"] = metric
        if rows:
            verdict["latest_commit"] = rows[-1]["commit"]["sha"]
            verdict["latest_source"] = rows[-1]["source"]["url"]
        verdicts.append(verdict)
    return verdicts


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=".")
    parser.add_argument("--rev", default="origin/metrics")
    parser.add_argument("--config", default=str(HERE / "ratchet.json"))
    args = parser.parse_args(argv)
    sys.path.insert(0, str(HERE))
    import record  # noqa: PLC0415
    config = json.loads(Path(args.config).read_text(encoding="utf-8"))
    records = record.all_records(record.read_store(Path(args.repo), args.rev))
    verdicts = evaluate(records, config)
    failed = [v for v in verdicts if v["status"] == "regressed"]
    for verdict in verdicts:
        metric = verdict["metric"]
        if verdict["status"] == "insufficient":
            print(f"  wait  {metric}: {verdict['points']} Boat point(s), needs {verdict['needed']}")
        else:
            mark = {"regressed": "FAIL", "improved": "good", "within": "ok"}[verdict["status"]]
            print(f"  {mark:<5} {metric}: {verdict['current']:g} vs baseline {verdict['baseline']:g} "
                  f"({verdict['change_pct']:+.1f}%, band ±{verdict['noise_pct']}%)")
            if verdict["status"] == "regressed":
                print(f"::error title=metric regressed::{metric} {verdict['change_pct']:+.1f}% beyond its "
                      f"±{verdict['noise_pct']}% band at {verdict.get('latest_commit')} ({verdict.get('latest_source')})")
    print(f"{len(verdicts)} tracked, {len(failed)} regressed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
