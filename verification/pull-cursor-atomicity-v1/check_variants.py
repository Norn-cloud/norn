#!/usr/bin/env python3
"""Static and Apalache negative-canary verifier for PullCursorAtomicity (#1923, Lane C2).

- `--static` checks manifest.json against pull-cursor-atomicity.qnt and the Rust
  SQLite conformance test without invoking Quint/Apalache.
- Default mode executes every negative mutant and reachability witness via
  scripts/quint_apalache.py (checking the structured `--out` status for `violation`,
  matching verification/hosted-timers-v1/check_variants.py).
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
TIMEOUT_SECONDS = 300
VARIANT_DIRECTORY = "pull-cursor-atomicity-variants"


class CanaryError(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise CanaryError(message)


def check_static(manifest: dict) -> list[tuple[str, list[str]]]:
    require(
        manifest.get("schemaVersion") == "norn.verification.pull-cursor-atomicity-v1/1",
        "unknown manifest schemaVersion",
    )
    require(
        manifest.get("quint") == "0.32.0" and manifest.get("apalache") == "0.56.1",
        "unpinned tool versions",
    )
    runs: list[tuple[str, list[str]]] = []
    for model in manifest["models"]:
        relative = Path(model["path"])
        require(
            not relative.is_absolute() and ".." not in relative.parts,
            f"bad model path {relative}",
        )
        path = HERE / relative
        require(path.is_file(), f"missing model {relative}")
        source = path.read_text(encoding="utf-8")
        require(
            re.search(rf"^module {re.escape(model['module'])} \{{", source, re.MULTILINE)
            is not None,
            f"{relative}: module {model['module']} not found",
        )

        def defined(kind: str, name: str) -> bool:
            return re.search(rf"^\s*{kind} {re.escape(name)}\b", source, re.MULTILINE) is not None

        invariants = model["invariants"]
        require(
            invariants and len(invariants) == len(set(invariants)),
            f"{relative}: invariants must be unique",
        )
        for name in invariants:
            require(defined("def", name), f"{relative}: invariant {name} not defined")

        checked: list[str] = []
        for check in model["checks"]:
            for inv in check["invariants"]:
                require(inv in invariants, f"{relative}: check invariant {inv} undeclared")
            checked.extend(check["invariants"])
        require(
            sorted(set(checked) - {"typeOk"}) == sorted(set(invariants) - {"typeOk"}),
            f"{relative}: nominal checks must cover every declared non-typeOk invariant",
        )

        refuted = set()
        for variant in model["variants"]:
            require(
                variant["invariant"] in invariants,
                f"{relative}: variant targets undeclared invariant {variant['invariant']}",
            )
            require(
                defined("action", variant["step"]),
                f"{relative}: step {variant['step']} not defined",
            )
            require(
                variant["step"] != "step",
                f"{relative}: variant step must differ from nominal step",
            )
            require(
                variant.get("meaning", "").strip() != "",
                f"{relative}: variant {variant['step']} missing meaning",
            )
            refuted.add(variant["invariant"])
            label = f"{path.stem}-{variant['step']}-{variant['invariant']}"
            runs.append(
                (
                    label,
                    [
                        "quint",
                        "verify",
                        f"verification/pull-cursor-atomicity-v1/{model['path']}",
                        "--backend",
                        "apalache",
                        "--init",
                        variant.get("init", "init"),
                        "--step",
                        variant["step"],
                        "--invariant",
                        variant["invariant"],
                        "--max-steps",
                        str(variant["maxSteps"]),
                    ],
                )
            )
        missing = sorted(set(invariants) - {"typeOk"} - refuted)
        require(not missing, f"{relative}: invariants without a refuted mutant: {missing}")

        for witness in model["witnesses"]:
            name = witness["invariant"]
            require(
                name.startswith("witness") and defined("def", name),
                f"{relative}: witness {name} not defined",
            )
            label = f"{path.stem}-witness-{name}"
            runs.append(
                (
                    label,
                    [
                        "quint",
                        "verify",
                        f"verification/pull-cursor-atomicity-v1/{model['path']}",
                        "--backend",
                        "apalache",
                        "--init",
                        witness.get("init", "init"),
                        "--step",
                        witness.get("step", "step"),
                        "--invariant",
                        name,
                        "--max-steps",
                        str(witness["maxSteps"]),
                    ],
                )
            )

        conformance_test = ROOT / model["conformance"]["test"]
        require(conformance_test.is_file(), f"missing conformance test {conformance_test}")
    require(len({label for label, _ in runs}) == len(runs), "duplicate canary label")
    return runs


def refute(label: str, command: list[str], result_root: Path) -> float:
    out = result_root / VARIANT_DIRECTORY / f"{label}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.unlink(missing_ok=True)
    started = time.monotonic()
    completed = subprocess.run(
        [
            sys.executable,
            str(HARNESS),
            "--label",
            label,
            "--timeout",
            str(TIMEOUT_SECONDS),
            "--result-root",
            str(result_root),
            "--",
            *command,
            "--out",
            str(out),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    seconds = time.monotonic() - started
    try:
        status = json.loads(out.read_text(encoding="utf-8")).get("status")
    except (OSError, ValueError):
        status = None
    if completed.returncode == 0 or status != "violation":
        tail = "\n".join((completed.stdout + completed.stderr).splitlines()[-40:])
        raise CanaryError(
            f"{label}: expected a violation, got exit {completed.returncode}, status {status!r}\n{tail}"
        )
    return seconds


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--static",
        action="store_true",
        help="check manifest/model consistency without invoking Apalache",
    )
    args = parser.parse_args()
    try:
        manifest = json.loads((HERE / "manifest.json").read_text(encoding="utf-8"))
        runs = check_static(manifest)
        if args.static:
            print(
                f"pull-cursor-atomicity-v1 static check OK ({len(runs)} variant/witness runs declared)"
            )
            return 0
        result_root = Path(
            os.environ.get("NORN_VERIFICATION_RESULT_ROOT", ROOT / "target/verification-results")
        )
        for label, cmd in runs:
            seconds = refute(label, cmd, result_root)
            print(f"[refuted] {label} ({seconds:.1f}s)", flush=True)
    except CanaryError as error:
        print(f"[fail] {error}", file=sys.stderr)
        return 1
    print(f"pull-cursor-atomicity-v1: all {len(runs)} mutants and witnesses refuted")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
