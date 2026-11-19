#!/usr/bin/env python3
"""Negative canaries for the hosted timers and approval fencing models (#1464).

Each variant removes one mechanism of the design (the round-1 bug shape Sol found) and must
be refuted: Apalache must report a violation of the named invariant. Each witness is a
reachability probe written as an invariant that must also be violated, so the checked
invariants are not vacuous. A timeout, a type error or any other non-violation outcome
fails the canary.

Every run goes through scripts/quint_apalache.py (pinned server, watchdogs, verdict-first
classification). `--static` checks only that manifest.json, the model sources and
promoted-gates.json agree.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
HARNESS = ROOT / "scripts/quint_apalache.py"
GATES = ROOT / "verification/coverage-v1/promoted-gates.json"
TIMEOUT_SECONDS = 300
VARIANT_DIRECTORY = "hosted-timers-variants"


class CanaryError(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise CanaryError(message)


def check_command(model: dict, check: dict) -> str:
    step = f"--step {check['step']} " if "step" in check else ""
    if check.get("mode") == "simulate":
        return (
            f"quint run verification/hosted-timers-v1/{model['path']} {step}"
            f"--invariant {' '.join(check['invariants'])} --max-steps {check['maxSteps']} "
            f"--max-samples {check['samples']} --seed {check['seed']}"
        )
    return (
        f"quint verify verification/hosted-timers-v1/{model['path']} --backend apalache "
        f"{step}--invariants {' '.join(check['invariants'])} --max-steps {check['maxSteps']}"
    )


def check_static(manifest: dict) -> list[tuple[str, list[str]]]:
    """Check the manifest against the models and gates; return the runs to refute."""
    require(manifest.get("schemaVersion") == "norn.verification.hosted-timers-v1/1", "unknown manifest schema")
    require(manifest.get("quint") == "0.32.0" and manifest.get("apalache") == "0.56.1", "unpinned tool versions")
    gates = {gate["id"]: gate for gate in json.loads(GATES.read_text(encoding="utf-8"))["gates"]}
    runs: list[tuple[str, list[str]]] = []
    for model in manifest["models"]:
        relative = Path(model["path"])
        require(not relative.is_absolute() and ".." not in relative.parts, f"bad model path {relative}")
        path = HERE / relative
        require(path.is_file(), f"missing model {relative}")
        source = path.read_text(encoding="utf-8")
        require(re.search(rf"^module {re.escape(model['module'])} \{{", source, re.MULTILINE) is not None,
                f"{relative}: module {model['module']} not found")

        def defined(kind: str, name: str) -> bool:
            return re.search(rf"^\s*{kind} {re.escape(name)}\b", source, re.MULTILINE) is not None

        invariants = model["invariants"]
        require(invariants and len(invariants) == len(set(invariants)), f"{relative}: invariants must be unique")
        for name in invariants:
            require(defined("def", name), f"{relative}: invariant {name} not defined")
        typecheck = gates.get(f"{path.stem}-quint-typecheck")
        require(typecheck is not None and typecheck["command"] == f"quint typecheck verification/hosted-timers-v1/{model['path']}",
                f"{path.stem}-quint-typecheck: missing or disagrees with the manifest")
        checked = []
        for check in model["checks"]:
            gate = gates.get(check["gate"])
            require(gate is not None, f"{check['gate']}: no promoted gate")
            require(gate["command"] == check_command(model, check), f"{check['gate']}: gate command disagrees with the manifest")
            results = [] if check.get("mode") == "simulate" else [f"target/verification-results/{check['gate']}.json"]
            require(gate["resultArtifacts"] == results, f"{check['gate']}: result artifact disagrees with the manifest")
            require(gate["profile"] == check["profile"], f"{check['gate']}: profile disagrees with the manifest")
            require("step" not in check or defined("action", check["step"]), f"{check['gate']}: step not defined")
            if check.get("mode") != "simulate":
                checked += check["invariants"]
        require(sorted(set(checked) - {"typeOk"}) == sorted(set(invariants) - {"typeOk"}),
                f"{relative}: every invariant must be gated exactly as declared")
        refuted = set()
        for variant in model["variants"]:
            require(variant["invariant"] in invariants, f"{relative}: variant checks an ungated invariant")
            require(defined("action", variant["step"]), f"{relative}: step {variant['step']} not defined")
            init = variant.get("init", "init")
            require(defined("action", init), f"{relative}: init {init} not defined")
            require(variant["step"] != "step" or init != "init", f"{relative}: a variant must change the design")
            require(variant.get("meaning", "").strip() != "", f"{relative}: variant needs a meaning")
            refuted.add(variant["invariant"])
            label = f"{path.stem}-{variant['step']}-{init}-{variant['invariant']}"
            runs.append((label, [
                "quint", "verify", f"verification/hosted-timers-v1/{model['path']}", "--backend", "apalache",
                "--init", init, "--step", variant["step"], "--invariant", variant["invariant"],
                "--max-steps", str(variant["maxSteps"]),
            ]))
        missing = sorted(set(invariants) - {"typeOk"} - refuted)
        require(not missing, f"{relative}: invariants without a refuted variant: {missing}")
        for witness in model["witnesses"]:
            name = witness["invariant"]
            require(name.startswith("witness") and defined("def", name), f"{relative}: witness {name} not defined")
            label = f"{path.stem}-witness-{name}"
            runs.append((label, [
                "quint", "verify", f"verification/hosted-timers-v1/{model['path']}", "--backend", "apalache",
                "--invariant", name, "--max-steps", str(witness["maxSteps"]),
            ]))
    require(len({label for label, _ in runs}) == len(runs), "duplicate canary label")
    return runs


def refute(label: str, command: list[str], result_root: Path) -> float:
    out = result_root / VARIANT_DIRECTORY / f"{label}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.unlink(missing_ok=True)
    started = time.monotonic()
    completed = subprocess.run(
        [sys.executable, str(HARNESS), "--label", label, "--timeout", str(TIMEOUT_SECONDS),
         "--result-root", str(result_root), "--", *command, "--out", str(out)],
        cwd=ROOT, capture_output=True, text=True,
    )
    seconds = time.monotonic() - started
    try:
        status = json.loads(out.read_text(encoding="utf-8")).get("status")
    except (OSError, ValueError):
        status = None
    if completed.returncode == 0 or status != "violation":
        tail = "\n".join((completed.stdout + completed.stderr).splitlines()[-40:])
        raise CanaryError(f"{label}: expected a violation, got exit {completed.returncode}, status {status!r}\n{tail}")
    return seconds


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--static", action="store_true", help="check manifest, models and gates only")
    arguments = parser.parse_args()
    try:
        manifest = json.loads((HERE / "manifest.json").read_text(encoding="utf-8"))
        runs = check_static(manifest)
        if arguments.static:
            print(f"hosted-timers-v1: {len(runs)} canaries declared and consistent")
            return 0
        result_root = Path(os.environ.get("NORN_VERIFICATION_RESULT_ROOT", ROOT / "target/verification-results"))
        for label, command in runs:
            seconds = refute(label, command, result_root)
            print(f"[refuted] {label} ({seconds:.1f}s)", flush=True)
    except CanaryError as error:
        print(f"[fail] {error}", file=sys.stderr)
        return 1
    print(f"hosted-timers-v1: all {len(runs)} variants and witnesses refuted")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
