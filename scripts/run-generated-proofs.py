#!/usr/bin/env python3
"""Trusted outer process boundary for the checker-owned proof job producer/binder."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def asset_pins(assets):
    pins = {}
    for name, path in assets:
        if not name or Path(name).is_absolute() or ".." in Path(name).parts or name in pins:
            raise ValueError("invalid or duplicate logical tool asset")
        pins[name] = sha(path)
    return pins


def main():
    if len(sys.argv) != 8:
        raise ValueError("usage: run-generated-proofs.py BINARY ORIGINAL MUTANT BASE BOUNDS WORK TOOLS")
    binary, *arguments = sys.argv[1:]
    tools = Path(arguments[-1])
    quint = tools / "bin/quint"
    located_bun = shutil.which("bun")
    bun = str(Path(located_bun).resolve()) if located_bun else None
    if not bun or subprocess.check_output([quint, "--version"]).strip() != b"0.32.0" or subprocess.check_output([bun, "--version"]).strip() != b"1.3.14":
        raise ValueError("backend version mismatch")
    package = Path("tools/norn-proofs")
    if json.loads((package / "node_modules/tla-ts/package.json").read_text())["version"] != "0.0.7":
        raise ValueError("tla-ts version mismatch")
    python_version = ".".join(map(str, sys.version_info[:3]))
    assets = [(f"python/{python_version}/interpreter", Path(sys.executable)), ("bun/1.3.14/bun", Path(bun)),
              ("quint/0.32.0/quint", quint), ("temurin/21.0.12.1_1/java", tools / "bin/java")]
    apalache = tools / "quint-home/apalache-dist-0.56.1/apalache"
    assets += [(f"apalache/0.56.1/{name}", apalache / name) for name in ["bin/apalache-mc", "lib/apalache.jar"]]
    assets += [(f"norn-proof-runner/1/{name}", path) for name, path in [
        ("scripts/run-generated-proofs.py", Path(__file__)),
        ("scripts/check-generated-quint.py", Path("scripts/check-generated-quint.py")),
        ("scripts/quint_apalache.py", Path("scripts/quint_apalache.py")),
        ("check.ts", package / "check.ts"), ("bun.lock", package / "bun.lock")]]
    for name, version in [("tla-ts", "0.0.7"), ("fflate", "0.8.3")]:
        root = package / "node_modules" / name
        if json.loads((root / "package.json").read_text())["version"] != version:
            raise ValueError(f"{name} version mismatch")
        assets += [(f"{name}/{version}/{path.relative_to(root).as_posix()}", path)
                   for path in sorted(root.rglob("*")) if path.is_file() and
                   (path.name == "package.json" or path.suffix in {".js", ".mjs", ".cjs"})]
    control = {"toolchain": asset_pins(assets), "results": {}}
    plan = json.loads(subprocess.check_output([binary, "prepare", *arguments], input=json.dumps(control).encode()))
    for job in plan["jobs"]:
        if sha(job["model"]) != job["modelSha"]:
            raise ValueError("generated model changed")
        subprocess.run([sys.executable, "scripts/check-generated-quint.py", "--model", job["model"], "--out", job["result"],
                        "--tools", str(tools), "--bun", bun, "--step", job["step"], "--invariant", job["invariant"], "--seed", str(job["seed"]),
                        "--expected-states", str(job["expectedStates"])], check=True)
        control["results"][job["key"]] = {"resultSha": sha(job["result"]), "modelSha": job["modelSha"], "challenge": job["challenge"]}
    bound = subprocess.run([binary, "bind", *arguments], input=json.dumps(control).encode())
    return bound.returncode


if __name__ == "__main__":
    sys.exit(main())
