#!/usr/bin/env python3
"""Regenerate the bundle packer's duration table from measured CI bundle logs.

Usage (from the repository root; needs an authenticated `gh`):

    python3 -I crates/norn-skeleton-guard/scripts/bundle_durations.py [--runs N | --run ID ...] \
        > crates/norn-skeleton-guard/src/ci/bundle-durations.json

By default it reads the last 10 successful merge-group runs of the `Public core` workflow:
those run the full plan, so every task is measured in every run. Each bundle job prints
`::group::<task>` when a task starts and `::notice::task=<task> result=success seconds=<n>`
when it ends (crates/norn-skeleton-guard/src/ci/bundle.rs); GitHub keeps a timestamp on every
log line. The table is the median over those runs of:

- `test_build_seconds`: the workspace test build the first Test-group task of a bundle (a
  test shard or `phase/test-shards`) pays, from the task's start to nextest's "Finished `test`
  profile", less the rebuild every later test shard of the same bundle still pays (its
  median, which stays in each task's own time);
- `wasm_pkg_seconds`: the checker WASM package build, from the start of the tasks that begin
  with it (`phase/browser`, `phase/integration-wasm`) to the package's size report. Only the
  first of a bundle's package tasks builds it; the later ones reuse it and log no report;
- `task_seconds`: each task's own time, net of the shared builds above (a Test-group task net
  of its measured build; the first WASM-package task of a bundle net of its measured build,
  or of the median package build for `phase/integration-authoring`, which builds it after its
  native binaries, so its log marks no start for the package build; a later WASM-package task,
  which reuses the package, as measured).

The packer (bundle.rs, and the hosted planner in scripts/ci/core.sh) charges each shared
build once per bundle that holds a task of its group, so a cold bundle costs exactly what
was measured. Never edit the table by hand: rerun this script.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import subprocess
import sys
from datetime import datetime

REPO = "Norn-cloud/norn"
WORKFLOW = "Public core"
TASK = r"((?:test|phase)/[a-z0-9-]+)"
STAMP = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d+))?Z (.*)$")
GROUP = re.compile(rf"^(?:##\[group\]|::group::){TASK}$")
NOTICE = re.compile(rf"^(?:##\[notice\]|::notice::)task={TASK} result=success seconds=(\d+)$")
PRUNED = re.compile(rf"^(?:##\[notice\]|::notice::)pruned target before {TASK};")
TEST_BUILT = re.compile(r"^\s*Finished `test` profile ")
WASM_BUILT = re.compile(r"^@norn/checker-wasm \S+ size report ")
ANSI = re.compile(r"\x1b\[[0-9;]*m")
WASM_FIRST = ("phase/browser", "phase/integration-wasm")
WASM_AFTER_NATIVE = ("phase/integration-authoring",)
TEST_AFTER_PREREQ = ("phase/guards",)


def gh(*args: str) -> str:
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def latest_runs(count: int) -> list[int]:
    runs = json.loads(gh("run", "list", "--repo", REPO, "--workflow", WORKFLOW, "--event", "merge_group",
                         "--status", "success", "--limit", str(count), "--json", "databaseId"))
    return sorted(run["databaseId"] for run in runs)


def bundle_logs(run: int) -> list[str]:
    jobs = json.loads(gh("run", "view", str(run), "--repo", REPO, "--json", "jobs"))["jobs"]
    ids = sorted(job["databaseId"] for job in jobs if job["name"].startswith("bundle ("))
    if not ids:
        sys.exit(f"run {run} has no bundle jobs")
    return [gh("api", "--allow-escape-sequences", f"repos/{REPO}/actions/jobs/{job}/logs") for job in ids]


def parse_bundle(log: str) -> list[tuple[str, int, list[tuple[float, str]], bool]]:
    """The tasks of one bundle log: (task, seconds, timestamped lines, pruned_before)."""
    tasks, current, pruned_before = [], None, None
    for raw in log.splitlines():
        match = STAMP.match(ANSI.sub("", raw))
        if not match:
            continue
        # The runner stamps every line in UTC (`Z`), whatever the reader's local zone.
        seconds = datetime.fromisoformat(f"{match[1]}+00:00").timestamp() + float(f"0.{match[2] or 0}")
        line = match[3]
        if pruned := PRUNED.match(line):
            pruned_before = pruned[1]
        elif group := GROUP.match(line):
            current = (group[1], [(seconds, line)], pruned_before == group[1])
            pruned_before = None
        elif notice := NOTICE.match(line):
            if current is None or current[0] != notice[1]:
                sys.exit(f"task {notice[1]} ended without starting")
            tasks.append((notice[1], int(notice[2]), current[1], current[2]))
            current = None
        elif current is not None:
            current[1].append((seconds, line))
    return tasks


def parse(log: str) -> list[tuple[str, int, list[tuple[float, str]]]]:
    """The tasks of one bundle log: (task, seconds, timestamped lines from its start)."""
    return [(task, seconds, lines) for task, seconds, lines, _ in parse_bundle(log)]


def elapsed(lines: list[tuple[float, str]], pattern: re.Pattern[str]) -> float | None:
    start = lines[0][0]
    return next((at - start for at, line in lines if pattern.match(line)), None)


def table(runs: list[int]) -> dict[str, object]:
    net: dict[str, list[float]] = {}
    cold_tests, warm_rebuilds, guards_cold, wasm_builds, after_native = [], [], [], [], []
    for run in runs:
        for log in bundle_logs(run):
            # bundle.rs runs a bundle's Test-group and WASM-package tasks back to back without
            # pruning, so only the first of each group builds its shared artifacts; the rest reuse
            # them.
            previous, wasm_built = None, False
            for task, seconds, lines, pruned in parse_bundle(log):
                if pruned:
                    previous = None
                    wasm_built = False
                own = float(seconds)
                tests = task.startswith("test/") or task in {"phase/test-shards", "phase/guards"}
                first, previous = tests and not previous, tests
                if first:
                    if task in TEST_AFTER_PREREQ:
                        # phase/guards runs fast checks before compiling workspace tests, so net
                        # it against the median shared workspace test build below.
                        guards_cold.append((task, own))
                    else:
                        # Charged below, once the shared part of its build is known.
                        cold_tests.append((task, own, elapsed(lines, TEST_BUILT) or 0.0))
                    continue
                if task.startswith("test/"):
                    warm_rebuilds.append(elapsed(lines, TEST_BUILT) or 0.0)
                if task in WASM_FIRST + WASM_AFTER_NATIVE and not wasm_built:
                    wasm_built = True
                    if task in WASM_AFTER_NATIVE:
                        after_native.append((task, own))
                        continue
                    build = elapsed(lines, WASM_BUILT)
                    if build is None:
                        sys.exit(f"run {run}: {task} logged no checker WASM size report")
                    wasm_builds.append(build)
                    own -= build
                net.setdefault(task, []).append(own)
    if not cold_tests or not warm_rebuilds or not wasm_builds:
        sys.exit("no cold test, warm test or WASM package build measured")
    rebuild = statistics.median(warm_rebuilds)
    test_builds = [max(0.0, build - rebuild) for _, _, build in cold_tests]
    for task, own, build in cold_tests:
        net.setdefault(task, []).append(own - max(0.0, build - rebuild))
    test_build = statistics.median(test_builds)
    for task, own in guards_cold:
        net.setdefault(task, []).append(max(0.0, own - test_build))
    wasm = statistics.median(wasm_builds)
    for task, own in after_native:
        net.setdefault(task, []).append(own - wasm)
    counts = {len(samples) for samples in net.values()}
    if len(counts) != 1:
        sys.exit(f"tasks measured unevenly: {sorted((task, len(samples)) for task, samples in net.items())}")
    return {
        "generated_by": "crates/norn-skeleton-guard/scripts/bundle_durations.py",
        "runs": runs,
        "statistic": "median",
        "test_build_seconds": round(test_build),
        "wasm_pkg_seconds": round(wasm),
        "task_seconds": {task: max(1, round(statistics.median(samples))) for task, samples in sorted(net.items())},
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--runs", type=int, default=10, help="latest successful merge-group runs to read")
    source.add_argument("--run", type=int, action="append", help="a run id to read (repeatable)")
    arguments = parser.parse_args()
    runs = sorted(set(arguments.run)) if arguments.run else latest_runs(arguments.runs)
    sys.stdout.write(json.dumps(table(runs), indent=2) + "\n")


if __name__ == "__main__":
    main()
