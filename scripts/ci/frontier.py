#!/usr/bin/env python3
"""CI Pareto frontier for the `Public core` gates.

    python3 scripts/ci/frontier.py collect RUN_ID[:ATTEMPT]... > .github/ci/frontier.json
    python3 scripts/ci/frontier.py check .github/ci/frontier.json

`collect` reads run, job, step and PR timeline timings through `gh api`. v3 records every paginated per-attempt job, including cancelled service and
reclamation overlap, job queue time and READY-to-plan latency. Merge throughput
counts distinct merged PRs in the measured window; it is separate from successful
qualification identities. Missing READY timestamps remain unknown. Missing job
queue timestamps remain unknown; pre-start intervals include dependencies and
are reported separately, never substituted for runner queue time.
`check` recomputes the statistics from the stored rows offline and
fails on drift, so the summary can never disagree with its evidence.

Time to verdict is the attempt's start to the last job's completion (queue + wall).
The historical v2 series includes only first attempts: a rerun keeps the run's original
`created_at`, so measuring it would count the wait before the rerun as queue
time. Reruns are counted in their own `reruns` column instead, split into
`harness` (the first attempt was refused by the harness) and `flake` (the rerun
reached a different verdict on the same inputs).
Percentiles interpolate linearly between closest ranks. The noise band is a
seeded bootstrap 95% interval of each percentile; with n=10, p95 sits between
the two slowest rows, so its band is wide by construction.
"""

import base64
import json
import re
import random
import statistics
import subprocess
import sys
from datetime import datetime

REPO = "Norn-cloud/norn"
SCHEMA = "norn.ci-frontier/v2"
WORKFLOW = ".github/workflows/ci-policy.yml"
RUN_NAME = "${{ github.event_name == 'pull_request' && github.event.action == 'ready_for_review' && format('READY {0} {1} {2}', github.event.pull_request.head.sha, github.event.pull_request.updated_at, github.sha) || github.event_name == 'issue_comment' && github.event.comment.user.type == 'Bot' && format('issue_comment bot {0}', github.event.comment.id) || github.event_name }}"
RESAMPLES = 2000
RUNNER = "ubuntu-24.04"


def gh(path, raw=False):
    flags = ["--allow-escape-sequences"] if raw else []
    out = subprocess.run(["gh", "api", *flags, f"repos/{REPO}/{path}"],
                         check=True, capture_output=True, text=True).stdout
    return out if raw else json.loads(out)


def ts(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()


def pages(path, key=None):
    """Read every API page, including jobs beyond the first 100."""
    result = []
    page = 1
    while True:
        data = gh(f"{path}{'&' if '?' in path else '?'}per_page=100&page={page}")
        batch = data[key] if key else data
        result.extend(batch)
        if len(batch) < 100:
            return result
        page += 1


def duration(start, end):
    return max(0, ts(end) - ts(start)) if start and end else 0


def row(spec):
    run_id, _, attempt = str(spec).partition(":")
    attempt = int(attempt or 1)
    run = gh(f"actions/runs/{run_id}/attempts/{attempt}")
    if run["path"].split("@", 1)[0] != WORKFLOW or run["status"] != "completed":
        raise ValueError(f"run {run_id}: expected completed Public core attempt")
    jobs = pages(f"actions/runs/{run_id}/attempts/{attempt}/jobs", "jobs")
    if len({j["id"] for j in jobs}) != len(jobs):
        raise ValueError("duplicate job identity")
    created = run["created_at"] if attempt == 1 else run["run_started_at"]
    ready = None
    attribution = re.fullmatch(r"READY ([0-9a-f]{40}) (\S+) ([0-9a-f]{40})", run.get("display_title", ""))
    if attempt == 1 and run["event"] == "pull_request" and attribution and attribution[1] == run["head_sha"]:
        for pr in run.get("pull_requests", []):
            if pr.get("head", {}).get("sha") != run["head_sha"]:
                continue
            try:
                revision = gh(f"git/ref/pull/{pr['number']}/merge")["object"]["sha"]
                if revision != attribution[3]:
                    continue
                content = gh(f"contents/{WORKFLOW}?ref={revision}")
                source = base64.b64decode(content["content"]).decode("utf-8")
                if "run-name: " + RUN_NAME in source.splitlines():
                    ts(attribution[2])
                    ready = attribution[2]
                    break
            except (subprocess.CalledProcessError, KeyError, ValueError, UnicodeError):
                # Optional attribution must not discard otherwise valid job timings.
                continue
    entries = []
    for job in jobs:
        start, end = job.get("started_at"), job.get("completed_at")
        steps = [{"name": step["name"], "conclusion": step["conclusion"],
                  "s": duration(step.get("started_at"), step.get("completed_at"))}
                 for step in job.get("steps", [])]
        service = duration(start, end) if job["conclusion"] != "skipped" else 0
        entries.append({"id": job["id"], "name": job["name"], "conclusion": job["conclusion"],
                        "created_at": job.get("created_at"), "started_at": start, "completed_at": end,
                        "service_s": service,
                        "queue_s": (duration(job["created_at"], start) if job.get("created_at") else None)
                        if job["conclusion"] != "skipped" else 0,
                        "pre_start_s": duration(created, start) if job["conclusion"] != "skipped" else 0,
                        "cancelled_s": service if job["conclusion"] == "cancelled" else 0,
                        "reclaim_s": sum(step["s"] for step in steps
                                         if step["name"] == "Reclaim runner disk space"),
                        "steps": steps})
    plan_starts = [j["started_at"] for j in entries if j["name"] == "plan" and j["started_at"]]
    return {"run_id": run["id"], "attempt": attempt, "event": run["event"],
            "head_sha": run["head_sha"], "created_at": created,
            "completed_at": run["updated_at"], "conclusion": run["conclusion"],
            "ready_at": ready,
            "admitted_at": min(plan_starts) if plan_starts else None, "jobs": entries}


def attempt_document(rows, merges):
    identities = set()
    for r in rows:
        identity = (r["run_id"], r["attempt"])
        if identity in identities:
            raise ValueError("duplicate run attempt")
        identities.add(identity)
        if len({j["id"] for j in r["jobs"]}) != len(r["jobs"]):
            raise ValueError("duplicate job")
        for job in r["jobs"]:
            if min(job[k] for k in ("service_s", "cancelled_s", "reclaim_s", "pre_start_s")) < 0:
                raise ValueError("negative job duration")
            expected = duration(job["started_at"], job["completed_at"]) if job["conclusion"] != "skipped" else 0
            queue = (duration(job["created_at"], job["started_at"]) if job["created_at"] else None) if job["conclusion"] != "skipped" else 0
            pre_start = duration(r["created_at"], job["started_at"]) if job["conclusion"] != "skipped" else 0
            if job["pre_start_s"] != pre_start:
                raise ValueError("pre-start interval drift")
            if job["queue_s"] != queue:
                raise ValueError("job queue drift")
            if job["service_s"] != expected or job["cancelled_s"] != (expected if job["conclusion"] == "cancelled" else 0):
                raise ValueError("job service/cancellation drift")
            if job["reclaim_s"] != sum(s["s"] for s in job["steps"] if s["name"] == "Reclaim runner disk space"):
                raise ValueError("reclaim drift")
            if sum(s["s"] for s in job["steps"]) > expected:
                raise ValueError("steps exceed job service")
    rows = sorted(rows, key=lambda r: (r["created_at"], r["run_id"], r["attempt"]))
    start = min((r["created_at"] for r in rows), default=None)
    end = max((r["completed_at"] for r in rows), default=None)
    merged = {}
    for pr in merges:
        if not start or not start <= pr["merged_at"] <= end:
            raise ValueError("merge outside measurement window")
        if pr["number"] in merged and pr != merged[pr["number"]]:
            raise ValueError("conflicting merge identity")
        merged[pr["number"]] = pr
    jobs = [j for r in rows for j in r["jobs"]]
    executed = [j for j in jobs if j["conclusion"] != "skipped" and j["started_at"]]
    # Reclaim inside a cancelled job is included in both costs, never added twice.
    totals = {key: sum(j[key] for j in jobs)
              for key in ("service_s", "cancelled_s", "reclaim_s", "pre_start_s")}
    queued = [j for j in executed if j["queue_s"] is not None]
    totals["known_queue_s"] = sum(j["queue_s"] for j in queued)
    totals["unknown_queue_jobs"] = len(executed) - len(queued)
    totals["queue_s"] = totals["known_queue_s"] if not totals["unknown_queue_jobs"] else None
    totals["cancelled_attempt_service_s"] = sum(j["service_s"] for r in rows
                                                if r["conclusion"] == "cancelled" for j in r["jobs"])
    totals["retained_service_s"] = totals["service_s"] - totals["cancelled_s"]
    totals["cancelled_share"] = totals["cancelled_s"] / totals["service_s"] if totals["service_s"] else 0
    admission = [duration(r["ready_at"], r["admitted_at"]) for r in rows if r["ready_at"] and r["admitted_at"]]
    window = duration(start, end)
    qualifications = {(r["head_sha"], r["event"]) for r in rows
                      if r["conclusion"] == "success" and any(j["name"] == "full" and j["conclusion"] == "success" for j in r["jobs"])}
    return {"schema": "norn.ci-frontier/v3", "repo": REPO, "workflow": "Public core",
            "rows": rows, "merges": sorted(merged.values(), key=lambda pr: pr["number"]),
            "summary": {**totals, "attempts": len(rows), "jobs": len(jobs), "executing_jobs": len(executed),
                        "window_start": start, "window_end": end, "window_s": window,
                        "cancelled_attempts": sum(r["conclusion"] == "cancelled" for r in rows),
                        "attempt_elapsed_s": dist([duration(r["created_at"], r["completed_at"]) for r in rows]) if rows else None,
                        "admission_latency_s": dist(admission) if admission else None,
                        "job_service_s": dist([j["service_s"] for j in executed]) if executed else None,
                        "job_queue_s": dist([j["queue_s"] for j in queued]) if queued else None,
                        "successful_qualifications": len(qualifications),
                        "merged_prs": len(merged), "merges_per_hour": len(merged) * 3600 / window if window else 0}}


def collect_merges(rows):
    start = min(r["created_at"] for r in rows)
    end = max(r["completed_at"] for r in rows)
    result = []
    page = 1
    while True:
        batch = gh(f"pulls?state=closed&sort=updated&direction=desc&per_page=100&page={page}")
        result += [{"number": pr["number"], "merged_at": pr["merged_at"]} for pr in batch
                   if pr.get("merged_at") and start <= pr["merged_at"] <= end]
        if len(batch) < 100 or batch[-1]["updated_at"] < start:
            return result
        page += 1


def self_test():
    from unittest.mock import patch
    stamp = "2026-11-02T00:00:00Z"
    finish = "2026-11-02T00:01:00Z"
    job = {"id": 1, "name": "full", "conclusion": "cancelled", "started_at": stamp,
           "created_at": "2026-11-01T23:59:50Z", "completed_at": finish, "service_s": 60, "queue_s": 10, "cancelled_s": 60,
           "reclaim_s": 20, "pre_start_s": 0, "steps": [{"name": "Reclaim runner disk space", "s": 20}]}
    first = {"run_id": 1, "attempt": 1, "event": "pull_request", "head_sha": "head",
             "created_at": stamp, "completed_at": finish, "conclusion": "cancelled",
             "ready_at": stamp, "admitted_at": finish, "jobs": [job]}
    import copy
    second = copy.deepcopy(first)
    second.update(attempt=2, conclusion="success")
    second["jobs"][0].update(id=2, conclusion="success", cancelled_s=0)
    merge = {"number": 7, "merged_at": finish}
    result = attempt_document([first, second], [merge, merge])["summary"]
    assert result["service_s"] == 120 and result["cancelled_s"] == 60
    assert result["cancelled_attempt_service_s"] == 60 and result["cancelled_attempts"] == 1
    assert result["retained_service_s"] == 60 and result["reclaim_s"] == 40
    assert result["queue_s"] == 20 and result["merged_prs"] == 1 and result["merges_per_hour"] == 60
    assert result["admission_latency_s"]["p50"] == 60 and result["successful_qualifications"] == 1
    unknown = copy.deepcopy(first)
    unknown["jobs"][0].update(created_at=None, queue_s=None)
    incomplete = attempt_document([unknown], [])["summary"]
    assert incomplete["queue_s"] is None and incomplete["unknown_queue_jobs"] == 1
    assert incomplete["known_queue_s"] == 0 and incomplete["service_s"] == 60
    assert incomplete["job_queue_s"] is None
    for mutate in (lambda r: r.append(r[0]), lambda r: r[0]["jobs"].append(r[0]["jobs"][0]),
                   lambda r: r[0]["jobs"][0].update(service_s=61),
                   lambda r: r[0]["jobs"][0].update(reclaim_s=21)):
        broken = copy.deepcopy([first])
        mutate(broken)
        try:
            attempt_document(broken, [])
        except ValueError:
            pass
        else:
            raise AssertionError("metrics corruption accepted")
    with patch.dict(globals(), gh=lambda path: {"jobs": list(range(100)) if path.endswith("&page=1") else [100]}):
        assert pages("actions/runs/1/attempts/2/jobs", "jobs") == list(range(101))
    def fixture_api(path):
        if path == "actions/runs/3/attempts/2":
            return {"id": 3, "path": WORKFLOW, "status": "completed", "created_at": stamp,
                    "run_started_at": finish, "updated_at": finish, "event": "pull_request",
                    "head_sha": "head", "conclusion": "success", "pull_requests": []}
        if path.startswith("actions/runs/3/attempts/2/jobs?"):
            ids = range(100) if path.endswith("&page=1") else [100]
            return {"jobs": [{"id": i, "name": str(i), "conclusion": "skipped"} for i in ids]}
        raise AssertionError(f"unexpected endpoint: {path}")
    with patch.dict(globals(), gh=fixture_api):
        collected = row("3:2")
        assert len(collected["jobs"]) == 101 and collected["created_at"] == finish
        summary = attempt_document([collected], [])["summary"]
        assert summary["service_s"] == 0 and summary["job_queue_s"] is None and summary["executing_jobs"] == 0
    ready_title = "READY " + "a" * 40 + " " + stamp + " " + "c" * 40
    for attempt, name, pinned, availability, expected in (
        (1, ready_title, True, "available", stamp),
        (2, ready_title, True, "available", None),
        (1, ready_title, False, "available", None),
        (1, "pull_request", True, "available", None),
        (1, ready_title.replace("a" * 40, "b" * 40), True, "available", None),
        (1, ready_title, True, "missing-ref", None),
        (1, ready_title, True, "missing-source", None),
        (1, ready_title, True, "moved-ref", None),
    ):
        def ready_api(path):
            if path == f"actions/runs/4/attempts/{attempt}":
                return {"id": 4, "path": WORKFLOW, "status": "completed", "created_at": finish,
                        "run_started_at": finish, "updated_at": finish, "event": "pull_request",
                        "head_sha": "a" * 40, "conclusion": "success", "display_title": name,
                        "pull_requests": [{"number": 7, "head": {"sha": "a" * 40}}]}
            if path == "git/ref/pull/7/merge":
                if availability == "missing-ref":
                    raise subprocess.CalledProcessError(1, "gh", stderr="HTTP 404")
                return {"object": {"sha": ("d" if availability == "moved-ref" else "c") * 40}}
            if path == "contents/" + WORKFLOW + "?ref=" + "c" * 40:
                if availability == "missing-source":
                    raise subprocess.CalledProcessError(1, "gh", stderr="HTTP 404")
                body = "run-name: " + RUN_NAME if pinned else "name: Public core"
                return {"content": base64.b64encode(body.encode()).decode()}
            if "/jobs?" in path:
                return {"jobs": [{"id": 9, "name": "phase", "conclusion": "success",
                                  "started_at": finish, "completed_at": finish}]}
            raise AssertionError(f"unattributed timeline or raw-head workflow lookup: {path}")
        with patch.dict(globals(), gh=ready_api):
            observed = row(f"4:{attempt}")
            assert observed["ready_at"] == expected and observed["jobs"][0]["queue_s"] is None
            assert observed["jobs"][0]["pre_start_s"] == 0
    with patch.dict(globals(), row=lambda spec: {"run_id": int(spec.split(":")[0]), "attempt": int(spec.split(":")[1])}):
        assert len(collect_attempts(["1:2", "1:2", "1:1"])) == 2
    print("frontier: pagination, attempt deduplication and cost reconciliation passed")


def collect_attempts(specs):
    unique = set()
    for spec in specs:
        run, _, attempt = str(spec).partition(":")
        unique.add((int(run), int(attempt or 1)))
    return [row(f"{run}:{attempt}") for run, attempt in sorted(unique)]


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


def document(rows, merges=None):
    if merges is not None or (rows and "jobs" in rows[0]):
        return attempt_document(rows, merges or [])
    validate_rows(rows)
    rows = sorted(rows, key=lambda r: (r["created_at"], r["run_id"], r["attempt"]))
    return {"schema": SCHEMA, "repo": REPO, "workflow": "Public core",
            "runner": f"{RUNNER} GitHub-hosted (public repository: $0)",
            "rows": rows, "series": series(rows)}


def main(argv):
    if argv == ["self-test"]:
        self_test()
        return 0
    if len(argv) >= 2 and argv[0] == "collect":
        rows = collect_attempts(argv[1:])
        json.dump(document(rows, collect_merges(rows)), sys.stdout, indent=1)
        sys.stdout.write("\n")
        return 0
    if len(argv) == 2 and argv[0] == "check":
        stored = json.load(open(argv[1], encoding="utf-8"))
        try:
            recomputed = document(stored["rows"], stored.get("merges"))
        except ValueError as error:
            print(f"{argv[1]}: {error}", file=sys.stderr)
            return 1
        if stored.get("schema") not in (SCHEMA, "norn.ci-frontier/v3") or stored != recomputed:
            print(f"{argv[1]}: series drift from rows; rerun collect", file=sys.stderr)
            return 1
        return 0
    print(__doc__.strip().splitlines()[2:4], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
