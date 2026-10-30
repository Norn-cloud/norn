#!/usr/bin/env python3
"""CI Pareto frontier for the hosted `Public core` gates (Norn-cloud/norn-platform#3541).

    python3 scripts/ci/frontier.py collect RUN_ID... > .github/ci/frontier.json
    python3 scripts/ci/frontier.py check .github/ci/frontier.json

`collect` reads run, job and step timings (and, for failed jobs, the job log)
through `gh api`, classifies each verdict and writes rows plus series
statistics. `check` recomputes the statistics from the stored rows offline and
fails on drift, so the summary can never disagree with its evidence.

Time to verdict is run creation to the last job's completion (queue + wall).
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
SCHEMA = "norn.ci-frontier/v1"
RESAMPLES = 2000
# Log or annotation substring -> cause. First match wins; order is specific to general.
CAUSES = [
    ("recent account payments have failed", "harness-billing"),
    ("exceeds timeout-minutes budget", "known-norn#9"),
    ("Diff in /", "known-norn#8"),
    ("public-cut .github/workflows/ci-policy.yml", "scratch-workflow-edit"),
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
    for needle, cause in CAUSES:
        if any(needle in text for text in texts):
            return cause
    return "harness-other" if not job["steps"] else "unknown"


def row(run_id):
    run = gh(f"actions/runs/{run_id}")
    jobs = [j for j in gh(f"actions/runs/{run_id}/jobs")["jobs"] if j["conclusion"] != "skipped"]
    if run["status"] != "completed" or len(jobs) != 1:
        raise SystemExit(f"run {run_id}: expected one completed gate job, got {len(jobs)}")
    job = jobs[0]
    created, started, completed = ts(run["created_at"]), ts(job["started_at"]), ts(job["completed_at"])
    failed = [s["name"] for s in job["steps"] if s["conclusion"] == "failure"]
    return {
        "run_id": run["id"], "attempt": run["run_attempt"], "event": run["event"],
        "ref": run["head_branch"], "head_sha": run["head_sha"], "gate": job["name"],
        "created_at": run["created_at"], "queue_s": round(started - created),
        "wall_s": round(completed - started), "verdict_s": round(completed - created),
        "conclusion": job["conclusion"], "failed_step": failed[0] if failed else None,
        "cause": classify(job),
        "steps": [{"name": s["name"], "conclusion": s["conclusion"],
                   "s": round(ts(s["completed_at"]) - ts(s["started_at"]))}
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


def series(rows):
    groups = {}
    for r in rows:
        groups.setdefault((r["ref"], r["head_sha"], r["gate"]), []).append(r)
    out = []
    for (ref, sha, gate), rs in sorted(groups.items()):
        measured = [r for r in rs if not r["cause"].startswith("harness")]
        causes = {}
        for r in rs:
            causes[r["cause"]] = causes.get(r["cause"], 0) + 1
        steps = {}
        for r in measured:
            for s in r["steps"]:
                steps.setdefault(s["name"], []).append(s)
        verdicts = {r["conclusion"] for r in measured}
        out.append({
            "ref": ref, "head_sha": sha, "gate": gate, "n": len(rs), "n_measured": len(measured),
            "causes": causes,
            # Same inputs, different verdicts: every minority verdict is a flake.
            "flake_rate": round(1 - max(sum(r["conclusion"] == v for r in measured) for v in verdicts)
                                / len(measured), 3) if measured else None,
            "verdict_s": dist([r["verdict_s"] for r in measured]) if measured else None,
            "queue_s": dist([r["queue_s"] for r in measured]) if measured else None,
            "wall_s": dist([r["wall_s"] for r in measured]) if measured else None,
            "steps": {name: {"n": len(ss), "p50_s": pct([s["s"] for s in ss], 0.5),
                             "p95_s": pct([s["s"] for s in ss], 0.95),
                             "fail_rate": round(sum(s["conclusion"] == "failure" for s in ss) / len(ss), 3)}
                      for name, ss in steps.items()},
        })
    return out


def document(rows):
    rows = sorted(rows, key=lambda r: (r["created_at"], r["run_id"]))
    return {"schema": SCHEMA, "repo": REPO, "workflow": "Public core",
            "runner": "ubuntu-24.04 GitHub-hosted (public repository: $0)",
            "rows": rows, "series": series(rows)}


def main(argv):
    if len(argv) >= 2 and argv[0] == "collect":
        json.dump(document([row(int(r)) for r in argv[1:]]), sys.stdout, indent=1)
        sys.stdout.write("\n")
        return 0
    if len(argv) == 2 and argv[0] == "check":
        stored = json.load(open(argv[1], encoding="utf-8"))
        if stored.get("schema") != SCHEMA or stored != document(stored["rows"]):
            print(f"{argv[1]}: series drift from rows; rerun collect", file=sys.stderr)
            return 1
        return 0
    print(__doc__.strip().splitlines()[2:4], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
