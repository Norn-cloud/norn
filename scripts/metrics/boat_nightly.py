#!/usr/bin/env python3
"""Measure main on one Boat VM and append the results to the metrics branch.

Runs from a machine with an authenticated `boat` CLI and a clone of this repository that
can push the metrics branch (the controller; schedule it with cron, see --help). It:

1. fetches origin/main and exports that exact commit (tracked files only);
2. `boat run --type default` from a Norn toolchain snapshot, pushing the export and running
   `scripts/metrics/run_probes.py` (CARGO_INCREMENTAL=0); `boat run` deletes the VM when the
   command ends, and this script then checks `boat info <id>` reports it gone;
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
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import record  # noqa: E402
import report  # noqa: E402

REPOSITORY = "Norn-cloud/norn"
SNAPSHOT = "norn-ci-main-a8c384a14d-v1"
SANDBOX = re.compile(r"\bbx_[a-z0-9]{6,}\b")


def boat(args: argparse.Namespace, *rest: str, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run([*args.boat.split(), *rest], text=True, **kwargs)


def export(repo: Path, sha: str, into: Path) -> Path:
    tree = into / "norn"
    tree.mkdir()
    archive = into / "src.tar"
    record.git(repo, "archive", "--format=tar", "-o", str(archive), sha)
    with tarfile.open(archive) as tar:
        unsafe = [m.name for m in tar.getmembers() if m.issym() or m.islnk() or m.name.startswith(("/", ".."))
                  or "/../" in m.name]
        if unsafe:
            raise SystemExit(f"refusing to upload links or unsafe paths: {unsafe[:5]}")
        tar.extractall(tree, filter="data")
    archive.unlink()
    return tree


def records_from(output: str, sha: str, date: str, sandbox: str | None, args) -> tuple[list[dict], list[str]]:
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
        workload = {**row.get("workload", {}), "boat_snapshot": args.snapshot}
        if sandbox:
            workload["boat_sandbox"] = sandbox
        item = {
            "metric": row["metric"], "value": row["value"], "unit": row["unit"], "statistic": row["statistic"],
            "commit": {"sha": sha, "date": date, "ref": "main"}, "measured_at": row["measured_at"],
            "measured_by": {"name": f"metrics-nightly-boat/{row['probe']}"},
            "env": {**env, "runner": "boat", "vm_type": args.type},
            "workload": workload, "caveats": row["caveats"], "estimate": False,
            "source": {"url": f"https://github.com/{REPOSITORY}/commit/{sha}"},
        }
        if "samples" in row:
            item["samples"] = row["samples"]
        if sandbox:
            item["id"] = f"boat/{sandbox}/{row['metric']}"
        records.append(item)
    return records, notes


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    parser.add_argument("--repo", default=".", help="clone of Norn-cloud/norn that can push the metrics branch")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--ref", default="main")
    parser.add_argument("--snapshot", default=SNAPSHOT, help="Boat snapshot with the pinned Rust/Bun toolchain")
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
    if not limits.get("canStart") or limits["starts"]["day"]["remaining"] < 1:
        print(f"boat cannot start a VM now: {limits.get('startBlockedReason') or 'no starts left'}", file=sys.stderr)
        return 1
    with tempfile.TemporaryDirectory(prefix="norn-metrics-") as scratch:
        tree = export(repo, sha, Path(scratch))
        # The probe runner and list are this script's own copies, so the cron's main
        # checkout decides what is measured whatever ref is exported.
        tools = tree / "scripts" / "metrics"
        tools.mkdir(parents=True, exist_ok=True)
        for name in ("run_probes.py", "record.py", "record.schema.json"):
            shutil.copyfile(HERE / name, tools / name)
        shutil.copyfile(args.probes, tools / "probes.json")
        command = ["python3", "scripts/metrics/run_probes.py"]
        for name in args.only or []:
            command += ["--only", name]
        ran = boat(args, "run", "--from", args.snapshot, "--type", args.type, "--timeout", str(args.timeout),
                   "--no-env", "--push", str(tree), "-e", "CARGO_INCREMENTAL=0",
                   "-e", f"RUSTUP_TOOLCHAIN={rust}", "--", *command,
                   capture_output=True)
    output = ran.stdout
    sys.stderr.write(ran.stderr[-4000:])
    sandbox = next(iter(SANDBOX.findall(ran.stdout + ran.stderr)), None)
    if sandbox:
        info = boat(args, "info", sandbox, "--json", capture_output=True)
        gone = info.returncode != 0 and "not_found" in (info.stdout + info.stderr).replace(" ", "_").lower()
        print(f"sandbox {sandbox}: {'deleted (boat info: not_found)' if gone else 'STILL PRESENT, delete it'}",
              file=sys.stderr)
        if not gone:
            boat(args, "delete", sandbox, "--yes")
    records, notes = records_from(output, sha, date, sandbox, args)
    for note in notes:
        print(note, file=sys.stderr)
    if not records:
        print(f"no measurements (boat run exited {ran.returncode})", file=sys.stderr)
        return 1
    if args.dry_run:
        for item in records:
            print(json.dumps(item, sort_keys=True))
    else:
        recorded_at = record.now_utc()
        commit, fresh = record.append(repo, [record.complete(r, recorded_at) for r in records],
                                      remote=args.remote, views=report.render_files,
                                      message=f"metrics: nightly Boat probes of main {sha[:12]}")
        print(f"recorded {len(fresh)} measurement(s) of {sha[:12]} in {commit}")
    return 0 if ran.returncode == 0 and not any(n.startswith("FAIL") for n in notes) else 1


if __name__ == "__main__":
    sys.exit(main())
