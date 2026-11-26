#!/usr/bin/env python3
"""Run the nightly measurement probes (probes.json) on the machine that measures.

boat_nightly.py runs this inside a Boat VM at a checkout of main. It prints, on stdout:

    ENV {json}                     the machine (record.py env)
    METRIC {json}                  one measurement: probe, metric, value, unit, statistic, measured_at,
                                   workload, caveats (and samples when the probe gave them)
    SKIP <probe> <reason>          a probe whose files are not at this commit
    FAIL <probe> <reason>          a probe whose prepare or run failed; nothing is recorded for it

A probe's `run` is timed (wall clock) when it names a `timed` metric; lines its command
prints as `METRIC {json}` are passed through; `parse` turns other output lines into
metrics with a regex (a `value` group, and an optional `name` group for `{name}` in the id).
`expect` is a regex the output must match, or the probe fails and records nothing (it pins a
test count, so a renamed test cannot shrink a timed set). Exit 1 if any probe failed, after
running the rest.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent

def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module


record = _sibling("record")


def emit(kind: str, payload) -> None:
    print(f"{kind} {json.dumps(payload, sort_keys=True) if not isinstance(payload, str) else payload}", flush=True)


def segment(text: str) -> str:
    """A metric id segment from free text (a gate id)."""
    cleaned = re.sub(r"[^a-z0-9_-]+", "_", text.lower()).strip("_-")
    return cleaned or "unnamed"


def _run_bash(script: str, root: Path, timeout: int, *, capture: bool) -> subprocess.CompletedProcess:
    command = ["bash", "-o", "pipefail", "-ec", script]
    stdout_target = subprocess.PIPE if capture else sys.stderr
    stderr_target = subprocess.PIPE if capture else sys.stderr
    with subprocess.Popen(
        command,
        cwd=root,
        stdout=stdout_target,
        stderr=stderr_target,
        text=capture,
        start_new_session=True,
    ) as proc:
        try:
            stdout, stderr = proc.communicate(timeout=timeout)
            return subprocess.CompletedProcess(command, proc.returncode, stdout, stderr)
        except BaseException:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            raise


def run_probe(probe: dict, root: Path) -> bool:
    name = probe["name"]
    missing = [path for path in probe.get("requires", []) if not (root / path).exists()]
    if missing:
        emit("SKIP", f"{name} missing {', '.join(missing)}")
        return True
    timeout = probe.get("timeout_s", 1800)
    if probe.get("prepare"):
        try:
            prepared = _run_bash(probe["prepare"], root, timeout, capture=False)
        except subprocess.TimeoutExpired:
            emit("FAIL", f"{name} prepare exceeded {timeout} s")
            return False
        if prepared.returncode != 0:
            emit("FAIL", f"{name} prepare exited {prepared.returncode}")
            return False
    started = time.monotonic()
    try:
        ran = _run_bash(probe["run"], root, timeout, capture=True)
    except subprocess.TimeoutExpired:
        emit("FAIL", f"{name} run exceeded {timeout} s")
        return False
    seconds = time.monotonic() - started
    sys.stderr.write(ran.stdout[-20000:] + ran.stderr[-20000:])
    if ran.returncode != 0:
        emit("FAIL", f"{name} run exited {ran.returncode}")
        return False
    output = ran.stdout + "\n" + ran.stderr
    if probe.get("expect") and not re.search(probe["expect"], output, re.M):
        emit("FAIL", f"{name} output does not match its expect pattern {probe['expect']!r}")
        return False
    now = record.now_utc()
    base = {"probe": name, "measured_at": now, "workload": probe.get("workload", {}), "caveats": probe["caveats"]}
    emitted: list[dict] = []
    if probe.get("timed"):
        emitted.append({**base, **probe["timed"], "value": round(seconds, 2)})
    parse = probe.get("parse")
    pattern = re.compile(parse["regex"]) if parse else None
    for line in output.splitlines():
        if line.startswith("METRIC "):
            try:
                measured = json.loads(line[len("METRIC "):])
            except json.JSONDecodeError:
                emit("FAIL", f"{name} printed a malformed METRIC line")
                return False
            if not isinstance(measured, dict):
                emit("FAIL", f"{name} printed a non-object METRIC line")
                return False
            workload = {**base["workload"], **measured.pop("workload", {})}
            emitted.append({**base, **measured, "workload": workload})
        elif pattern and (match := pattern.search(line.strip())):
            found = match.groupdict()
            try:
                parsed_value = float(found["value"])
            except (KeyError, TypeError, ValueError):
                emit("FAIL", f"{name} printed a non-numeric parsed value")
                return False
            workload = dict(base["workload"])
            if found.get("name"):
                workload["gate_id"] = found["name"]
            emitted.append({**base, "metric": parse["metric"].format(name=segment(found.get("name") or "")),
                            "value": parsed_value, "unit": parse["unit"],
                            "statistic": parse["statistic"], "workload": workload})
    for metric_payload in emitted:
        emit("METRIC", metric_payload)
    return True


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--probes", default=str(HERE / "probes.json"))
    parser.add_argument("--root", default=str(HERE.parents[1]))
    parser.add_argument("--only", action="append", help="run only these probes")
    args = parser.parse_args(argv)
    probes = json.loads(Path(args.probes).read_text(encoding="utf-8"))["probes"]
    emit("ENV", record.detect_env())
    ok = True
    for probe in probes:
        if args.only and probe["name"] not in args.only:
            continue
        sys.stderr.write(f"== probe {probe['name']} at {dt.datetime.now(dt.timezone.utc).isoformat()}\n")
        ok = run_probe(probe, Path(args.root)) and ok
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
