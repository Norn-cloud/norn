#!/usr/bin/env python3
"""Per-task times of every completed `Public core` run on main, as metric records (JSONL).

core.sh's `bundle` command ends each task with a notice annotation,
`task=<kind>/<item> result=<result> seconds=<n>`; this reads those annotations through the
GitHub API (`gh api`, authenticated by GH_TOKEN) and emits one `ci.core.task.<kind>.<item>.s`
record per successful task, plus `ci.core.run.s` (first job start to last job end) per
successful run. Ids are `ci.core/<run>/<attempt>/<task>`, so re-sweeping a window appends
nothing twice. Main runs on GitHub-hosted runners: the numbers are recorded for trends and
never ratcheted.

    python3 scripts/metrics/collect_ci.py --days 2 > ci.jsonl
    python3 scripts/metrics/record.py add --file ci.jsonl
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path

REPOSITORY = "Norn-cloud/norn"
WORKFLOW = "ci-policy.yml"
NOTICE = re.compile(r"^task=((?:test|phase)/[a-z0-9][a-z0-9_-]*) result=(\w+) seconds=(\d+)$")
CAVEATS = (
    "GitHub-hosted ubuntu-24.04 runner (GitHub publishes 4 vCPU / 16 GB for public repositories; "
    "the CPU model varies by host), shared hardware, sccache hit rate varies run to run; one sample. "
    "Seconds come from core.sh's per-task notice (bash SECONDS, 1 s resolution) and exclude job setup "
    "and tool installs. Recorded for trends only, never ratcheted.")


def gh_lines(path: str, jq: str) -> list[dict]:
    out = subprocess.run(["gh", "api", "--paginate", path, "--jq", jq],
                         capture_output=True, text=True, check=True).stdout
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def toolchain(repo: Path, sha: str) -> dict:
    shown = subprocess.run(["git", "-C", str(repo), "show", f"{sha}:.github/workflows/ci-policy.yml"],
                           capture_output=True, text=True)
    match = re.search(r"^\s*toolchain:\s*([0-9][0-9.]*)\s*$", shown.stdout, re.M)
    return {"rust": match.group(1)} if shown.returncode == 0 and match else {}


def run_records(run: dict, jobs: list[dict], notices: dict[int, list[str]], repo: Path) -> list[dict]:
    sha, attempt = run["head_sha"], run["run_attempt"]
    commit = {"sha": sha, "date": run["head_commit"]["timestamp"], "ref": "main"}
    env = {"runner": "github-hosted", "vm_type": "ubuntu-24.04", "vcpu": 4, "ram_gib": 16, "os": "Ubuntu 24.04"}
    tools = toolchain(repo, sha)
    if tools:
        env["toolchain"] = tools
    base = {"schema": 1, "unit": "s", "statistic": "single", "commit": commit, "env": env,
            "caveats": CAVEATS, "estimate": False, "source": {"url": run["html_url"]}}
    records = []
    for job in jobs:
        for message in notices.get(job["id"], []):
            match = NOTICE.match(message.strip())
            if not match or match.group(2) != "success":
                continue
            task, seconds = match.group(1), int(match.group(3))
            kind, item = task.split("/", 1)
            records.append({**base, "id": f"ci.core/{run['id']}/{attempt}/{task}",
                            "metric": f"ci.core.task.{kind}.{item}.s", "value": seconds,
                            "measured_at": job["completed_at"],
                            "measured_by": {"name": "ci.core", "url": job["html_url"]},
                            "workload": {"event": run["event"], "plan": "full", "job": job["name"],
                                         "run_attempt": attempt}})
    ran = [job for job in jobs if job.get("started_at") and job.get("completed_at")
           and job.get("conclusion") not in (None, "skipped")]
    if run["conclusion"] == "success" and ran:
        start = min(job["started_at"] for job in ran)
        end = max(job["completed_at"] for job in ran)
        seconds = (dt.datetime.fromisoformat(end.replace("Z", "+00:00"))
                   - dt.datetime.fromisoformat(start.replace("Z", "+00:00"))).total_seconds()
        records.append({**base, "id": f"ci.core/{run['id']}/{attempt}/run", "metric": "ci.core.run.s",
                        "value": int(seconds), "measured_at": end,
                        "measured_by": {"name": "ci.core", "url": run["html_url"]},
                        "workload": {"event": run["event"], "plan": "full", "jobs": len(ran), "run_attempt": attempt},
                        "caveats": "First job start to last job end of a successful main run, queueing between "
                                   "jobs included. " + CAVEATS})
    return records


def collect(since: str, repo: Path, limit: int | None = None) -> list[dict]:
    runs = gh_lines(f"repos/{REPOSITORY}/actions/workflows/{WORKFLOW}/runs?branch=main&event=push"
                    f"&status=completed&per_page=100&created=%3E%3D{since}", ".workflow_runs[]")
    records = []
    for run in runs[:limit] if limit else runs:
        jobs = gh_lines(f"repos/{REPOSITORY}/actions/runs/{run['id']}/jobs?filter=latest&per_page=100", ".jobs[]")
        notices = {}
        for job in jobs:
            if job["name"].startswith("bundle (") and job.get("conclusion") not in (None, "skipped"):
                notices[job["id"]] = [a["message"] for a in gh_lines(
                    f"repos/{REPOSITORY}/check-runs/{job['id']}/annotations?per_page=100", ".[]")
                    if a.get("annotation_level") == "notice"]
        found = run_records(run, jobs, notices, repo)
        records += found
        print(f"run {run['id']}: {len(found)} records", file=sys.stderr)
    return records


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--days", type=int, default=2, help="runs created in the last N days")
    parser.add_argument("--since", help="ISO date; overrides --days")
    parser.add_argument("--repo", default=".", help="checkout used to read each run's pinned toolchain")
    parser.add_argument("--limit", type=int)
    args = parser.parse_args(argv)
    since = args.since or (dt.date.today() - dt.timedelta(days=args.days)).isoformat()
    for record in collect(since, Path(args.repo), args.limit):
        print(json.dumps(record, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
