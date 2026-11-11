#!/usr/bin/env python3
"""Measure main on one Boat VM and append the results to the metrics branch.

Runs from a machine with an authenticated `boat` CLI and a clone of this repository that
can push the metrics branch (the controller; schedule it with cron, see --help). It:

1. fetches origin/main and resolves its commit;
2. one `boat run --type default` per snapshot the probes name (probes.json): the VM fetches that
   exact commit from the public repository into a git checkout and runs this script's own
   `run_probes.py` against it (CARGO_INCREMENTAL=0; probes with `pin_rust` run on CI's pinned toolchain);
   `boat run` deletes the VM when the command ends, and this script then checks `boat info <id>`
   reports it gone, deleting it with --yes if not;
3. turns each METRIC line into a record (runner boat, the VM's own ENV line, the commit
   measured) and appends them with record.py, refreshing the trend view.

The hosted metrics-nightly workflow runs the ratchet over these Boat series afterwards, so a
regression turns that scheduled run red; nothing here gates a pull request.

    python3 scripts/metrics/boat_nightly.py --repo /root/norn            # measure and record
    python3 scripts/metrics/boat_nightly.py --repo /root/norn --dry-run  # measure, print records only

Cron on the controller (01:30 UTC, before the 03:17 UTC hosted nightly), running main's copy of
these tools without touching the clone's work tree:

    30 1 * * * git -C /root/norn fetch -q origin main && rm -rf /root/work/norn-metrics-tools &&
        mkdir -p /root/work/norn-metrics-tools && git -C /root/norn archive origin/main scripts/metrics |
        tar -x -C /root/work/norn-metrics-tools && python3 /root/work/norn-metrics-tools/scripts/metrics/boat_nightly.py
        --repo /root/norn >> /root/work/norn-metrics-nightly.log 2>&1
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import record  # noqa: E402
import report  # noqa: E402

REPOSITORY = "Norn-cloud/norn"
SANDBOX = re.compile(r"\bbx_[a-z0-9]{6,}\b")


def boat(args: argparse.Namespace, *rest: str, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run([*args.boat.split(), *rest], text=True, **kwargs)


def records_from(output: str, sha: str, date: str, sandbox: str | None, snapshot: str,
                 vm_type: str) -> tuple[list[dict], list[str]]:
    env, rows, notes = {}, [], []
    for line in output.splitlines():
        kind, _, payload = line.partition(" ")
        if kind == "ENV":
            env = json.loads(payload)
        elif kind == "METRIC":
            rows.append(json.loads(payload))
        elif kind in ("SKIP", "FAIL"):
            notes.append(line)
    records = []
    for row in rows:
        workload = {**row.get("workload", {}), "boat_snapshot": snapshot}
        if sandbox:
            workload["boat_sandbox"] = sandbox
        item = {
            "metric": row["metric"], "value": row["value"], "unit": row["unit"], "statistic": row["statistic"],
            "commit": {"sha": sha, "date": date, "ref": "main"}, "measured_at": row["measured_at"],
            "measured_by": {"name": f"metrics-nightly-boat/{row['probe']}"},
            "env": {**env, "runner": "boat", "vm_type": vm_type},
            "workload": workload, "caveats": row["caveats"], "estimate": False,
            "source": {"url": f"https://github.com/{REPOSITORY}/commit/{sha}"},
        }
        if "samples" in row:
            item["samples"] = row["samples"]
        if sandbox:
            item["id"] = f"boat/{sandbox}/{row['metric']}"
        records.append(item)
    return records, notes


def starts_record(limits: dict, at: str) -> dict:
    """Boat starts used in the rolling 24 h window, read before this run's own start."""
    day = limits["starts"]["day"]
    return {
        "id": f"boat.starts.used_24h/{at}", "metric": "boat.starts.used_24h", "value": day["used"], "unit": "count",
        "statistic": "single", "commit": {"sha": None, "date": None}, "measured_at": at,
        "measured_by": {"name": "metrics-nightly-boat"}, "env": {"runner": "local"},
        "workload": {"limit": day["limit"], "window": "rolling 24 h"},
        "caveats": "Account-wide: every project on this Boat account, not only Norn. Rolling 24-hour window from "
                   "`boat limits --json`, read before this run's own start. Not tied to a commit.",
        "estimate": False, "source": {"url": f"https://github.com/{REPOSITORY}/blob/main/scripts/metrics/boat_nightly.py"},
    }


def sandbox_gone(args, sandbox: str, attempts: int = 5) -> bool:
    """`boat info` reports not_found; an unreachable API is retried, never read as gone."""
    for attempt in range(attempts):
        info = boat(args, "info", sandbox, "--json", capture_output=True)
        text = (info.stdout + info.stderr).replace(" ", "_").lower()
        if info.returncode != 0 and "not_found" in text:
            return True
        if info.returncode == 0:
            return False
        time.sleep(10 * (attempt + 1))
    return False


def measure(args, repo: Path, sha: str, date: str, rust: str, snapshot: str,
            names: list[str]) -> tuple[list[dict], list[str], int]:
    """One `boat run` from `snapshot` for the probes `names`; boat deletes the VM when it ends."""
    with tempfile.TemporaryDirectory(prefix="norn-metrics-") as scratch:
        # Only this script's probe runner and list are uploaded, so the cron's main checkout
        # decides what is measured. The VM fetches the measured commit itself into a real git
        # checkout (the formal gate reads HEAD and checks every file in the tree).
        upload = Path(scratch) / "norn-metrics-run"
        tools = upload / "metrics-tools"
        tools.mkdir(parents=True)
        for name in ("run_probes.py", "record.py", "record.schema.json"):
            shutil.copyfile(HERE / name, tools / name)
        shutil.copyfile(args.probes, tools / "probes.json")
        only = " ".join(f"--only {shlex.quote(name)}" for name in names)
        script = (f"git init -q norn-measured && git -C norn-measured fetch -q --depth 1 "
                  f"https://github.com/{REPOSITORY}.git {sha} && git -C norn-measured checkout -q FETCH_HEAD && "
                  f"python3 metrics-tools/run_probes.py --root norn-measured --probes metrics-tools/probes.json {only}")
        ran = boat(args, "run", "--from", snapshot, "--type", args.type, "--timeout", str(args.timeout),
                   "--no-env", "--push", str(upload), "-e", "CARGO_INCREMENTAL=0",
                   "-e", f"METRICS_RUST_TOOLCHAIN={rust}", "--", "bash", "-c", script,
                   capture_output=True)
    sys.stderr.write(ran.stderr[-4000:])
    sandbox = next(iter(SANDBOX.findall(ran.stdout + ran.stderr)), None)
    if sandbox:
        gone = sandbox_gone(args, sandbox)
        if not gone:
            boat(args, "delete", sandbox, "--yes", capture_output=True)
            gone = sandbox_gone(args, sandbox)
        print(f"sandbox {sandbox} ({snapshot}): {'deleted (boat info: not_found)' if gone else 'NOT CONFIRMED DELETED'}",
              file=sys.stderr)
    records, notes = records_from(ran.stdout, sha, date, sandbox, snapshot, args.type)
    return records, notes, ran.returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    parser.add_argument("--repo", default=".", help="clone of Norn-cloud/norn that can push the metrics branch")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--ref", default="main")
    parser.add_argument("--snapshot", help="override probes.json's default snapshot (a probe's own wins)")
    parser.add_argument("--type", default="default", choices=["small", "default"])
    parser.add_argument("--timeout", type=int, default=6600, help="seconds before boat stops the command")
    parser.add_argument("--boat", default="boat", help="the boat CLI; it must run on this machine, because "
                        "`boat run --push` uploads a local directory")
    parser.add_argument("--only", action="append", help="run only these probes")
    parser.add_argument("--probes", default=str(HERE / "probes.json"), help="probe list to run")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    repo = Path(args.repo)
    record.git(repo, "fetch", "--quiet", "--no-tags", args.remote, args.ref)
    sha = record.git(repo, "rev-parse", "FETCH_HEAD^{commit}").stdout.decode().strip()
    date = record.git(repo, "show", "-s", "--format=%cI", sha).stdout.decode().strip()
    # The toolchain CI pins at this commit; the snapshot's default rustc may differ.
    workflow = record.git(repo, "show", f"{sha}:.github/workflows/ci-policy.yml").stdout.decode()
    rust = re.search(r"^\s*toolchain:\s*([0-9][0-9.]*)\s*$", workflow, re.M).group(1)
    limits = json.loads(boat(args, "limits", "--json", capture_output=True, check=True).stdout)
    starts_at = record.now_utc()
    if not limits.get("canStart") or limits["starts"]["day"]["remaining"] < 1:
        print(f"boat cannot start a VM now: {limits.get('startBlockedReason') or 'no starts left'}", file=sys.stderr)
        return 1
    probes = json.loads(Path(args.probes).read_text(encoding="utf-8"))
    groups: dict[str, list[str]] = {}
    for probe in probes["probes"]:
        if not args.only or probe["name"] in args.only:
            groups.setdefault(probe.get("snapshot", args.snapshot or probes["snapshot"]), []).append(probe["name"])
    records, notes, failed = [], [], False
    for snapshot, names in groups.items():
        found, seen, code = measure(args, repo, sha, date, rust, snapshot, names)
        records += found
        notes += seen
        failed = failed or code != 0
    for note in notes:
        print(note, file=sys.stderr)
    if not records:
        print("no probe measured anything", file=sys.stderr)
    records.append(starts_record(limits, starts_at))
    if args.dry_run:
        for item in records:
            print(json.dumps(item, sort_keys=True))
    else:
        recorded_at = record.now_utc()
        commit, fresh = record.append(repo, [record.complete(r, recorded_at) for r in records],
                                      remote=args.remote, views=report.render_files,
                                      message=f"metrics: nightly Boat probes of main {sha[:12]}")
        print(f"recorded {len(fresh)} measurement(s) of {sha[:12]} in {commit}")
    return 0 if not failed and not any(n.startswith("FAIL") for n in notes) else 1


if __name__ == "__main__":
    sys.exit(main())
