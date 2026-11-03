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
    assets = [Path(sys.executable), Path(bun), quint, tools / "bin/java", tools / "quint-home/apalache-dist-0.56.1/apalache/lib/apalache.jar",
              Path(__file__), Path("scripts/check-generated-quint.py"), Path("scripts/quint_apalache.py"), package / "check.ts", package / "bun.lock", package / "node_modules/tla-ts/package.json", package / "node_modules/fflate/package.json"]
    assets += sorted((package / "node_modules/tla-ts/dist").rglob("*.js"))
    assets += sorted(p for p in (package / "node_modules/fflate").rglob("*") if p.is_file() and p.suffix in {".js", ".mjs", ".cjs"})
    control = {"toolchain": {str(path): sha(path) for path in assets}, "results": {}}
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
