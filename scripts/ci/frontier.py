#!/usr/bin/env python3
"""CI Pareto frontier for the hosted `Public core` gates (Norn-cloud/norn-platform#3541).

    python3 scripts/ci/frontier.py collect RUN_ID[:ATTEMPT]... > .github/ci/frontier.json
    python3 scripts/ci/frontier.py check .github/ci/frontier.json

`collect` reads run, job and step timings (and, for failed jobs, the job log)
through `gh api`, classifies each verdict and writes rows plus series
statistics. `check` recomputes the statistics from the stored rows offline and
fails on drift, so the summary can never disagree with its evidence.

Time to verdict is the attempt's start to the last job's completion (queue + wall).
Only first attempts enter the timing series: a rerun keeps the run's original
`created_at`, so measuring it would count the wait before the rerun as queue
time. Reruns are counted in their own `reruns` column instead, split into
`harness` (the first attempt was refused by the harness) and `flake` (the rerun
reached a different verdict on the same inputs).
Percentiles interpolate linearly between closest ranks. The noise band is a
seeded bootstrap 95% interval of each percentile; with n=10, p95 sits between
the two slowest rows, so its band is wide by construction.
"""

import json
import random
import statistics
import subprocess
import sys
from datetime import datetime

REPO = "Norn-cloud/norn"
SCHEMA = "norn.ci-frontier/v2"
WORKFLOW = ".github/workflows/ci-policy.yml"
GATES = {"core", "trusted-policy"}
RESAMPLES = 2000
RUNNER = "ubuntu-24.04"
# Every substring present in one log or annotation -> cause. First match wins.
CAUSES = [
    (("recent account payments have failed",), "harness-billing"),
    (("has exceeded the maximum execution time",), "timeout"),
    (("exceeds timeout-minutes budget",), "known-norn#9"),
    (("Diff in /", "/crates/norn-skeleton-guard/src/ci/mod.rs:203:"), "known-norn#8"),
    (("Diff in /",), "fmt-other"),
    (("public-cut .github/workflows/ci-policy.yml",), "scratch-workflow-edit"),
]


def gh(path, raw=False):
    flags = ["--allow-escape-sequences"] if raw else []
    out = subprocess.run(["gh", "api", *flags, f"repos/{REPO}/{path}"],
                         check=True, capture_output=True, text=True).stdout
    return out if raw else json.loads(out)


def ts(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()


def classify(job):
    if job["conclusion"] == "success":
        return "pass"
    texts = [a["message"] for a in gh(f"check-runs/{job['id']}/annotations")]
    if any(s["conclusion"] == "failure" for s in job["steps"]):
        texts.append(gh(f"actions/jobs/{job['id']}/logs", raw=True))
    for needles, cause in CAUSES:
        if any(all(n in text for n in needles) for text in texts):
            return cause
    return "harness-other" if not job["steps"] else "unknown"


def row(spec):
    run_id, _, attempt = str(spec).partition(":")
    attempt = int(attempt or 1)
    run = gh(f"actions/runs/{run_id}/attempts/{attempt}")
    jobs = [j for j in gh(f"actions/runs/{run_id}/attempts/{attempt}/jobs")["jobs"]
            if j["conclusion"] != "skipped"]
    if (run["path"].split("@", 1)[0] != WORKFLOW or run["status"] != "completed" or len(jobs) != 1
            or jobs[0]["name"] not in GATES or jobs[0]["labels"] != [RUNNER]):
        raise SystemExit(f"run {run_id}: expected one completed Public core gate job")
    job = jobs[0]
    # A rerun keeps the first attempt's created_at; time each attempt from its own start.
    created = ts(run["created_at"] if run["run_attempt"] == 1 else run["run_started_at"])
    started, completed = ts(job["started_at"]), ts(job["completed_at"])
    failed = [s["name"] for s in job["steps"] if s["conclusion"] == "failure"]
    return {
        "run_id": run["id"], "attempt": run["run_attempt"], "event": run["event"],
        "ref": run["head_branch"], "head_sha": run["head_sha"], "gate": job["name"],
        "created_at": run["created_at"], "queue_s": round(started - created),
        "wall_s": round(completed - started), "verdict_s": round(completed - created),
        "conclusion": job["conclusion"], "failed_step": failed[0] if failed else None,
        "cause": classify(job),
        "steps": [{"name": s["name"], "conclusion": s["conclusion"],
                   "number": s["number"], "s": round(ts(s["completed_at"]) - ts(s["started_at"]))}
                  for s in job["steps"] if s.get("started_at") and s.get("completed_at")],
    }


def pct(values, q):
    v = sorted(values)
    k = (len(v) - 1) * q
    lo, hi = int(k), min(int(k) + 1, len(v) - 1)
    return round(v[lo] + (v[hi] - v[lo]) * (k - lo), 1)


def band(values, q):
    rng = random.Random(f"{q}:{len(values)}")
    est = sorted(pct(rng.choices(values, k=len(values)), q) for _ in range(RESAMPLES))
    return [est[int(0.025 * RESAMPLES)], est[int(0.975 * RESAMPLES) - 1]]


def dist(values):
    return {"p50": pct(values, 0.5), "p95": pct(values, 0.95),
            "p50_band95": band(values, 0.5), "p95_band95": band(values, 0.95),
            "mad": round(statistics.median(abs(x - statistics.median(values)) for x in values), 1),
            "min": min(values), "max": max(values)}


def verdict(r):
    """A job killed at its timeout-minutes reports `cancelled`; it is a failed gate run."""
    return "failure" if r["cause"] == "timeout" else r["conclusion"]


def validate_rows(rows):
    """Reject evidence the statistics cannot trust, before any summary is computed."""
    seen = set()
    for r in rows:
        key = (r["run_id"], r["attempt"])
        if key in seen:
            raise ValueError(f"run {r['run_id']} attempt {r['attempt']}: duplicate row")
        seen.add(key)
        if min(r["queue_s"], r["wall_s"], r["verdict_s"]) < 0:
            raise ValueError(f"run {r['run_id']} attempt {r['attempt']}: negative duration")
        # Each duration is rounded on its own, so the sum may differ by one second.
        if abs(r["verdict_s"] - (r["queue_s"] + r["wall_s"])) > 1:
            raise ValueError(f"run {r['run_id']} attempt {r['attempt']}: verdict_s != queue_s + wall_s")
    for r in rows:
        if r["attempt"] > 1 and (r["run_id"], 1) not in seen:
            raise ValueError(f"run {r['run_id']} attempt {r['attempt']}: rerun without its first attempt")


def series(rows):
    groups = {}
    for r in rows:
        groups.setdefault((r["ref"], r["head_sha"], r["gate"]), []).append(r)
    out = []
    for (ref, sha, gate), rs in sorted(groups.items()):
        # Cancelled, harness-refused and rerun rows stay as evidence but carry no timing.
        firsts = [r for r in rs if r["attempt"] == 1]
        measured = [r for r in firsts if verdict(r) in ("success", "failure")
                    and not r["cause"].startswith("harness")]
        first_of = {r["run_id"]: r for r in firsts}
        reruns = [r for r in rs if r["attempt"] > 1]
        harness = sum(first_of.get(r["run_id"], {}).get("cause", "").startswith("harness") for r in reruns)
        flake = sum(r["run_id"] in first_of and not first_of[r["run_id"]]["cause"].startswith("harness")
                    and verdict(first_of[r["run_id"]]) != verdict(r) for r in reruns)
        causes = {}
        for r in rs:
            causes[r["cause"]] = causes.get(r["cause"], 0) + 1
        steps = {}
        for r in measured:
            for s in r["steps"]:
                # Unnamed steps of the same action share a display name; the number tells them apart.
                key = f'{s["number"]}. {s["name"]}'
                steps.setdefault(key, [])
                if s["conclusion"] != "skipped":
                    steps[key].append(s)
        verdicts = {verdict(r) for r in measured}
        out.append({
            "ref": ref, "head_sha": sha, "gate": gate, "n": len(rs), "n_measured": len(measured),
            "causes": causes,
            "reruns": {"n": len(reruns), "harness": harness, "flake": flake},
            # Same inputs, different verdicts: every minority verdict is a flake.
            "flake_rate": round(1 - max(sum(verdict(r) == v for r in measured) for v in verdicts)
                                / len(measured), 3) if measured else None,
            "verdict_s": dist([r["verdict_s"] for r in measured]) if measured else None,
            "queue_s": dist([r["queue_s"] for r in measured]) if measured else None,
            "wall_s": dist([r["wall_s"] for r in measured]) if measured else None,
            "steps": {name: {"n": len(ss), "skipped": len(measured) - len(ss),
                             "p50_s": pct([s["s"] for s in ss], 0.5) if ss else None,
                             "p95_s": pct([s["s"] for s in ss], 0.95) if ss else None,
                             # A timeout cancels the running step; that is a failure.
                             "fail_rate": round(sum(s["conclusion"] in ("failure", "cancelled") for s in ss) / len(ss), 3)
                             if ss else None}
                      for name, ss in steps.items()},
        })
    return out


def document(rows):
    validate_rows(rows)
    rows = sorted(rows, key=lambda r: (r["created_at"], r["run_id"], r["attempt"]))
    return {"schema": SCHEMA, "repo": REPO, "workflow": "Public core",
            "runner": f"{RUNNER} GitHub-hosted (public repository: $0)",
            "rows": rows, "series": series(rows)}


def main(argv):
    if len(argv) >= 2 and argv[0] == "collect":
        rows = [row(r) for r in argv[1:]]
        # A rerun needs its first attempt for the harness/flake split: collect it too.
        have = {(r["run_id"], r["attempt"]) for r in rows}
        rows += [row(r["run_id"]) for r in list(rows) if r["attempt"] > 1 and (r["run_id"], 1) not in have]
        json.dump(document(rows), sys.stdout, indent=1)
        sys.stdout.write("\n")
        return 0
    if len(argv) == 2 and argv[0] == "check":
        stored = json.load(open(argv[1], encoding="utf-8"))
        try:
            recomputed = document(stored["rows"])
        except ValueError as error:
            print(f"{argv[1]}: {error}", file=sys.stderr)
            return 1
        if stored.get("schema") != SCHEMA or stored != recomputed:
            print(f"{argv[1]}: series drift from rows; rerun collect", file=sys.stderr)
            return 1
        return 0
    print(__doc__.strip().splitlines()[2:4], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
